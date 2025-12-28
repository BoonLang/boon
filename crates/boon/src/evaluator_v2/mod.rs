//! Evaluator for the new arena-based engine.
//!
//! Compiles AST expressions into reactive nodes in the arena.
//! (force rebuild v2)

use std::collections::HashMap;
use std::sync::Arc;
use crate::engine_v2::{
    arena::SlotId,
    event_loop::EventLoop,
    message::{Payload, FieldId},
    node::{EffectType, NodeKind, RuntimePattern, ComparisonOp},
    address::{ScopeId, SourceId, Port},
};
use crate::parser::{Expression, Spanned, Object, TextPart, Argument, Arm, Pattern, Literal};

/// Stored function definition for later invocation.
#[derive(Clone)]
pub struct FunctionDef<'code> {
    pub name: String,
    pub parameters: Vec<String>,
    pub body: Spanned<Expression<'code>>,
}

/// Context for compilation.
/// Tracks compile-time state for expression compilation.
pub struct CompileContext<'a, 'code> {
    pub event_loop: &'a mut EventLoop,
    /// Current scope for node instantiation
    pub scope_id: ScopeId,
    /// Local bindings: variable name → SlotId (lexical scope)
    pub local_bindings: HashMap<String, SlotId>,
    /// PASS/PASSED context stack (§2.11 - compile-time only)
    pub pass_stack: Vec<SlotId>,
    /// Function parameters: name → SlotId (for FUNCTION compilation)
    pub parameters: HashMap<String, SlotId>,
    /// Function definitions: name → FunctionDef
    pub functions: HashMap<String, FunctionDef<'code>>,
    /// Current variable placeholder being compiled (for early value setting)
    pub current_variable_slot: Option<SlotId>,
}

impl<'a, 'code> CompileContext<'a, 'code> {
    pub fn new(event_loop: &'a mut EventLoop) -> Self {
        Self {
            event_loop,
            scope_id: ScopeId::ROOT,
            local_bindings: HashMap::new(),
            pass_stack: Vec::new(),
            parameters: HashMap::new(),
            functions: HashMap::new(),
            current_variable_slot: None,
        }
    }

    /// Push PASS context for function call (§2.11).
    pub fn push_pass(&mut self, slot: SlotId) {
        self.pass_stack.push(slot);
    }

    /// Pop PASS context after function body compiled.
    pub fn pop_pass(&mut self) {
        self.pass_stack.pop();
    }

    /// Get current PASSED context (top of stack).
    pub fn current_passed(&self) -> Option<SlotId> {
        self.pass_stack.last().copied()
    }

    /// Add a call-arg local binding (§2.11 - compile-time only).
    pub fn add_local_binding(&mut self, name: String, slot: SlotId) {
        self.local_bindings.insert(name, slot);
    }

    /// Remove a local binding after function body compiled.
    pub fn remove_local_binding(&mut self, name: &str) {
        self.local_bindings.remove(name);
    }

    /// Resolve a variable name to its SlotId.
    pub fn resolve_variable(&self, name: &str) -> Option<SlotId> {
        // Check local bindings first (call-arg bindings)
        if let Some(&slot) = self.local_bindings.get(name) {
            return Some(slot);
        }
        // Then check function parameters
        if let Some(&slot) = self.parameters.get(name) {
            return Some(slot);
        }
        // Variable not found in current scope
        None
    }

    /// Compile a constant value.
    pub fn compile_constant(&mut self, value: Payload) -> SlotId {
        let slot = self.event_loop.arena.alloc();
        if let Some(node) = self.event_loop.arena.get_mut(slot) {
            // Use set_kind() for lazy extension allocation (§2.2.5.1)
            node.set_kind(NodeKind::Producer { value: Some(value.clone()) });
            // Also store in current_value for immediate reads via get_current_value()
            node.extension_mut().current_value = Some(value);
        }
        slot
    }

    /// Compile a wire (alias to another slot).
    pub fn compile_wire(&mut self, source: SlotId) -> SlotId {
        let slot = self.event_loop.arena.alloc();

        // Debug: Log when source is in template_input range
        #[cfg(target_arch = "wasm32")]
        if source.index >= 800 && source.index <= 850 {
            zoon::println!("compile_wire: source={:?} -> slot={:?} (template range!)", source, slot);
        }

        if let Some(node) = self.event_loop.arena.get_mut(slot) {
            node.set_kind(NodeKind::Wire { source: Some(source) });
        }
        // Subscribe to source (wire receives on its input port 0)
        self.event_loop.routing.add_route(source, slot, Port::Input(0));
        slot
    }

    /// Compile an object with fields.
    pub fn compile_object(&mut self, fields: Vec<(FieldId, SlotId)>) -> SlotId {
        let slot = self.event_loop.arena.alloc();
        let field_map: HashMap<FieldId, SlotId> = fields.into_iter().collect();

        if let Some(node) = self.event_loop.arena.get_mut(slot) {
            node.set_kind(NodeKind::Router { fields: field_map });
            // Set current_value to ObjectHandle so Wires can read it
            node.extension_mut().current_value = Some(Payload::ObjectHandle(slot));
        }

        slot
    }

    /// Get a field from a Router node, following Wire indirection if needed.
    pub fn get_field(&self, slot: SlotId, field: FieldId) -> Option<SlotId> {
        let node = self.event_loop.arena.get(slot)?;
        match node.kind()? {
            NodeKind::Router { fields } => fields.get(&field).copied(),
            NodeKind::Wire { source: Some(source_slot) } => {
                // Follow wire to find actual Router
                self.get_field(*source_slot, field)
            }
            NodeKind::Producer { value: Some(Payload::TaggedObject { fields: fields_slot, .. }) } => {
                // Tagged object - look inside its fields Router
                self.get_field(*fields_slot, field)
            }
            NodeKind::Producer { value: Some(Payload::ObjectHandle(router_slot)) } => {
                // ObjectHandle - look inside the referenced Router
                self.get_field(*router_slot, field)
            }
            _ => None,
        }
    }

    /// Follow Wire chain to find the actual IOPad slot.
    fn follow_wire_to_iopad(&self, slot: SlotId) -> Option<SlotId> {
        let node = self.event_loop.arena.get(slot)?;
        match node.kind()? {
            NodeKind::IOPad { .. } => Some(slot),
            NodeKind::Wire { source: Some(source_slot) } => {
                self.follow_wire_to_iopad(*source_slot)
            }
            _ => None,
        }
    }

    // Phase 4: Combinator compilation

    /// Compile a LATEST combinator (multi-input merge).
    pub fn compile_latest(&mut self, input_slots: Vec<SlotId>) -> SlotId {
        let slot = self.event_loop.arena.alloc();
        let input_count = input_slots.len();

        if let Some(node) = self.event_loop.arena.get_mut(slot) {
            node.set_kind(NodeKind::Combiner {
                inputs: input_slots.clone(),
                last_values: vec![None; input_count],
            });
        }

        // Wire each input to this combiner's numbered input ports
        for (i, input_slot) in input_slots.into_iter().enumerate() {
            self.event_loop.routing.add_route(input_slot, slot, Port::Input(i as u8));
        }

        slot
    }

    /// Compile a HOLD register (stateful accumulator).
    pub fn compile_hold(&mut self, initial_value: Payload, body_slot: Option<SlotId>, initial_input: Option<SlotId>) -> SlotId {
        let slot = self.event_loop.arena.alloc();

        if let Some(node) = self.event_loop.arena.get_mut(slot) {
            node.set_kind(NodeKind::Register {
                stored_value: Some(initial_value),
                body_input: body_slot,
                initial_input,
                initial_received: initial_input.is_none(), // If no initial input, mark as received
            });
        }

        // If there's a body, wire it to this register's input (Port::Input(0))
        if let Some(body) = body_slot {
            self.event_loop.routing.add_route(body, slot, Port::Input(0));
        }

        // If there's an initial input, wire it to this register (Port::Input(1))
        if let Some(init) = initial_input {
            self.event_loop.routing.add_route(init, slot, Port::Input(1));
        }

        slot
    }

    /// Compile a THEN transformer (copy on input arrival).
    pub fn compile_then(&mut self, input_slot: SlotId, body_slot: SlotId) -> SlotId {
        let slot = self.event_loop.arena.alloc();

        #[cfg(target_arch = "wasm32")]
        zoon::println!("compile_then: input_slot={:?} body_slot={:?} -> then_slot={:?}", input_slot, body_slot, slot);

        if let Some(node) = self.event_loop.arena.get_mut(slot) {
            node.set_kind(NodeKind::Transformer {
                input: Some(input_slot),
                body_slot: Some(body_slot),
            });
        }

        // Wire input to this transformer
        self.event_loop.routing.add_route(input_slot, slot, Port::Input(0));

        // Wire body_slot ONLY if it's a streaming construct (WHILE/SwitchedWire).
        // This is crucial for functions returning reactive values that produce
        // values asynchronously (like fibonacci's WHILE).
        //
        // Do NOT wire for simple expressions (like `state |> Bool/not()`) because
        // those would create spurious forwarding when dependencies change.
        let is_streaming_body = self.event_loop.arena.get(body_slot)
            .and_then(|n| n.kind())
            .map(|k| matches!(k, NodeKind::SwitchedWire { .. }))
            .unwrap_or(false);

        if is_streaming_body {
            #[cfg(target_arch = "wasm32")]
            zoon::println!("compile_then: wiring streaming body {:?} to {:?}", body_slot, slot);
            self.event_loop.routing.add_route(body_slot, slot, Port::Input(1));
        }

        slot
    }

    /// Compile a WHEN pattern matcher.
    pub fn compile_when(&mut self, input_slot: SlotId, arms: Vec<(RuntimePattern, SlotId)>) -> SlotId {
        let slot = self.event_loop.arena.alloc();

        if let Some(node) = self.event_loop.arena.get_mut(slot) {
            node.set_kind(NodeKind::PatternMux {
                input: Some(input_slot),
                current_arm: None,
                arms: arms.clone(),
            });
        }

        // Wire input to this pattern matcher
        self.event_loop.routing.add_route(input_slot, slot, Port::Input(0));

        // NOTE: We intentionally do NOT wire body slots to PatternMux.
        // WHEN is "one-shot copy" - it captures body value at match time.
        // Reactive body wiring causes race conditions where body updates
        // are forwarded before the input pattern re-evaluates.

        slot
    }

    /// Compile a WHILE switched wire.
    pub fn compile_while(&mut self, input_slot: SlotId, arms: Vec<(RuntimePattern, SlotId)>) -> SlotId {
        let slot = self.event_loop.arena.alloc();

        #[cfg(target_arch = "wasm32")]
        zoon::println!("compile_while: input_slot={:?} arms={} -> slot={:?}", input_slot, arms.len(), slot);

        // Evaluate initial arm based on input's current value
        let initial_arm = self.event_loop.get_current_value(input_slot)
            .and_then(|payload| {
                arms.iter().position(|(pattern, _)| pattern.matches(payload))
            });

        #[cfg(target_arch = "wasm32")]
        zoon::println!("  initial_arm={:?} input_value={:?}", initial_arm, self.event_loop.get_current_value(input_slot));

        // Set the WHILE node's current_value based on the initial arm's body
        let initial_value = initial_arm.and_then(|idx| {
            arms.get(idx).and_then(|(_, body_slot)| {
                let val = self.event_loop.get_current_value(*body_slot).cloned();
                #[cfg(target_arch = "wasm32")]
                {
                    let node_kind = self.event_loop.arena.get(*body_slot).and_then(|n| n.kind().cloned());
                    zoon::println!("  body_slot={:?} body_value={:?} node_kind={:?}", body_slot, val, node_kind);
                }
                val
            })
        });

        if let Some(node) = self.event_loop.arena.get_mut(slot) {
            node.set_kind(NodeKind::SwitchedWire {
                input: Some(input_slot),
                current_arm: initial_arm,
                arms,
            });
            // Set the current_value so rendering can access it
            if let Some(value) = initial_value {
                node.extension_mut().current_value = Some(value);
            }
        }

        // Wire input to this switched wire (Port::Input(0) for main input)
        self.event_loop.routing.add_route(input_slot, slot, Port::Input(0));

        // Wire each arm's body slot to the WHILE (Port::Input(1+i) for arm bodies)
        // This ensures body value updates flow to WHILE and can be forwarded
        if let Some(node) = self.event_loop.arena.get(slot) {
            if let Some(NodeKind::SwitchedWire { arms, .. }) = node.kind() {
                for (i, (_, body_slot)) in arms.iter().enumerate() {
                    self.event_loop.routing.add_route(*body_slot, slot, Port::Input(1 + i as u8));
                }
            }
        }

        slot
    }

    // Phase 6: Timer & Accumulator

    /// Compile a Timer/interval node.
    pub fn compile_timer(&mut self, interval_ms: f64) -> SlotId {
        let slot = self.event_loop.arena.alloc();

        #[cfg(target_arch = "wasm32")]
        zoon::println!("compile_timer: slot={:?} interval_ms={}", slot, interval_ms);

        if let Some(node) = self.event_loop.arena.get_mut(slot) {
            node.set_kind(NodeKind::Timer {
                interval_ms,
                next_tick: 1,
                active: true,
            });
        }

        // Schedule the first timer event
        self.event_loop.schedule_timer(slot, interval_ms);

        slot
    }

    /// Compile a Math/sum (Accumulator) node.
    pub fn compile_accumulator(&mut self, input_slot: SlotId) -> SlotId {
        let slot = self.event_loop.arena.alloc();

        if let Some(node) = self.event_loop.arena.get_mut(slot) {
            node.set_kind(NodeKind::Accumulator { sum: 0.0, has_input: false });
        }

        // Wire input to accumulator
        self.event_loop.routing.add_route(input_slot, slot, Port::Input(0));

        slot
    }

    /// Compile a Stream/pulses node that emits 0, 1, 2, ..., N-1 over N ticks.
    pub fn compile_pulses(&mut self, count: u32) -> SlotId {
        let slot = self.event_loop.arena.alloc();

        if let Some(node) = self.event_loop.arena.get_mut(slot) {
            node.set_kind(NodeKind::Pulses {
                total: count,
                current: 0,
                started: false,
            });
        }

        // Note: We don't mark_dirty here because the caller (CLI/browser runtime)
        // will mark all nodes dirty during initial evaluation. Marking twice
        // would cause double processing since dirty_nodes doesn't deduplicate.

        slot
    }

    /// Compile a Stream/skip node that skips the first N values from a source.
    pub fn compile_skip(&mut self, source: SlotId, count: u32) -> SlotId {
        let slot = self.event_loop.arena.alloc();

        if let Some(node) = self.event_loop.arena.get_mut(slot) {
            node.set_kind(NodeKind::Skip {
                source,
                count,
                skipped: 0,
                last_skipped_value: None,
            });
        }

        // Subscribe to the source
        self.event_loop.routing.add_route(source, slot, Port::Input(0));

        slot
    }

    // Phase 5: List compilation

    /// Compile a Bus (List) node.
    pub fn compile_list(&mut self, source_id: crate::engine_v2::address::SourceId) -> SlotId {
        use crate::engine_v2::node::AllocSite;

        let slot = self.event_loop.arena.alloc();

        if let Some(node) = self.event_loop.arena.get_mut(slot) {
            node.set_kind(NodeKind::Bus {
                items: Vec::new(),
                alloc_site: AllocSite::new(source_id),
                static_item_count: 0,  // Updated by compile_list_items after adding static items
            });
        }

        slot
    }

    // Phase 7d: TextTemplate compilation

    /// Compile a TEXT { literal {var} } template.
    pub fn compile_text_template(&mut self, template: String, dependency_slots: Vec<SlotId>) -> SlotId {
        let slot = self.event_loop.arena.alloc();

        // For static templates (no dependencies), pre-render immediately
        let cached = if dependency_slots.is_empty() {
            Some(Arc::from(template.as_str()))
        } else {
            None
        };

        if let Some(node) = self.event_loop.arena.get_mut(slot) {
            node.set_kind(NodeKind::TextTemplate {
                template,
                dependencies: dependency_slots.clone(),
                cached: cached.clone(),
            });
            // Also set current_value for get_slot_value to find
            if let Some(ref text) = cached {
                node.extension_mut().current_value = Some(Payload::Text(text.clone()));
            }
        }

        // Subscribe to all dependencies - each gets a distinct input port
        for (i, dep) in dependency_slots.iter().enumerate() {
            self.event_loop.routing.add_route(*dep, slot, Port::Input(i as u8));
        }

        slot
    }

    /// Compile a field access (for PASSED.field.path).
    pub fn compile_field_access(&mut self, source: SlotId, field_id: FieldId) -> SlotId {
        // Check if source is a Router and directly access the field slot
        if let Some(field_slot) = self.get_field(source, field_id) {
            // Debug: log completed field access
            #[cfg(target_arch = "wasm32")]
            {
                let field_name = self.event_loop.arena.get_field_name(field_id);
                if field_name.map(|n| n.as_ref()) == Some("completed") {
                    zoon::println!("COMPLETED_ACCESS: source={:?} -> field_slot={:?}", source, field_slot);
                }
            }
            // Direct access: wire to the field's actual slot
            return self.compile_wire(field_slot);
        }

        // Source is not a Router or field not found - create a dynamic field accessor
        // This handles cases where the source produces Router values at runtime
        let slot = self.event_loop.arena.alloc();

        #[cfg(target_arch = "wasm32")]
        zoon::println!("compile_field_access: dynamic extractor source={:?} field_id={:?} -> extractor_slot={:?}", source, field_id, slot);

        if let Some(node) = self.event_loop.arena.get_mut(slot) {
            node.set_kind(NodeKind::Extractor {
                source: Some(source),
                field: field_id,
                subscribed_field: None,
            });
        }

        // Route from source to this extractor
        self.event_loop.routing.add_route(source, slot, Port::Input(0));

        slot
    }

    /// Compile a top-level program (list of variable definitions).
    pub fn compile_program(&mut self, expressions: &[Spanned<Expression<'code>>]) -> Option<SlotId> {
        let mut last_slot = None;

        // Phase 0: Collect all function definitions first.
        // Functions can be defined anywhere and called from anywhere.
        for expr in expressions {
            if let Expression::Function { name, parameters, body } = &expr.node {
                let param_names: Vec<String> = parameters.iter()
                    .map(|p| p.node.to_string())
                    .collect();

                let func_def = FunctionDef {
                    name: name.to_string(),
                    parameters: param_names,
                    body: (**body).clone(),
                };

                self.functions.insert(name.to_string(), func_def);
            }
        }

        // Phase 1: Create placeholder wires for all top-level variable definitions.
        // This handles forward references (e.g., LIST { counter } before counter: ... is defined)
        for expr in expressions {
            if let Expression::Variable(var) = &expr.node {
                // Create a placeholder wire for this variable
                let placeholder_slot = self.event_loop.arena.alloc();
                if let Some(node) = self.event_loop.arena.get_mut(placeholder_slot) {
                    node.set_kind(NodeKind::Wire { source: None });
                }
                self.local_bindings.insert(var.name.to_string(), placeholder_slot);
            }
        }

        // Phase 1.5: Pre-allocate Routers and field placeholders for object-valued variables.
        // This is needed because Phase 2 compiles in reverse order, so when `document`
        // references `store.todos`, `store` hasn't been compiled yet. By pre-allocating
        // the Router and its field structure, we can resolve references at compile time.
        for expr in expressions {
            if let Expression::Variable(var) = &expr.node {
                if let Expression::Object(obj) = &var.value.node {
                    if let Some(&placeholder_slot) = self.local_bindings.get(var.name) {
                        // Pre-allocate Router and fields for this object
                        let router_slot = self.pre_allocate_object_structure(obj);
                        // Set placeholder's current_value to point to this Router
                        if let Some(node) = self.event_loop.arena.get_mut(placeholder_slot) {
                            node.extension_mut().current_value = Some(Payload::ObjectHandle(router_slot));
                        }
                        #[cfg(target_arch = "wasm32")]
                        zoon::println!("Phase 1.5: pre-allocated Router {:?} for variable '{}' (placeholder {:?})", router_slot, var.name, placeholder_slot);
                    }
                }
            }
        }

        // Phase 2: Compile expressions and wire them to the placeholders.
        // Process in forward order (source order). Phase 1 created placeholders so forward
        // references still work. Forward order ensures that when `document` references
        // `store.todos`, `store` has already been compiled.
        for expr in expressions.iter() {
            match &expr.node {
                Expression::Variable(var) => {
                    // Get the placeholder we created
                    let name: &str = var.name;
                    let placeholder_slot = self.local_bindings.get(name).copied();

                    // Track the current variable being compiled so HOLD can set its initial value early
                    self.current_variable_slot = placeholder_slot;

                    // Compile the variable's value
                    let value_slot = self.compile_expression(&var.value);

                    // Clear the tracking
                    self.current_variable_slot = None;

                    // Wire the placeholder to the compiled value
                    if let Some(placeholder) = placeholder_slot {
                        #[cfg(target_arch = "wasm32")]
                        zoon::println!("Variable '{}': placeholder={:?} value_slot={:?}", name, placeholder, value_slot);

                        if let Some(node) = self.event_loop.arena.get_mut(placeholder) {
                            if let Some(NodeKind::Wire { source }) = node.kind_mut() {
                                *source = Some(value_slot);
                            }
                        }
                        // Route from value source to wire so reactive updates propagate
                        self.event_loop.routing.add_route(value_slot, placeholder, Port::Input(0));
                        last_slot = Some(placeholder);
                    } else {
                        last_slot = Some(value_slot);
                    }
                }
                _ => {
                    // Top-level expression without binding
                    last_slot = Some(self.compile_expression(expr));
                }
            }
        }

        // If there's a "document" binding, return that (for UI apps).
        // Otherwise return the last expression (for CLI/scalar results).
        #[cfg(target_arch = "wasm32")]
        zoon::println!("compile_program: local_bindings={:?}", self.local_bindings);

        if let Some(doc_slot) = self.local_bindings.get("document").copied() {
            #[cfg(target_arch = "wasm32")]
            zoon::println!("  returning document slot={:?}", doc_slot);
            Some(doc_slot)
        } else {
            #[cfg(target_arch = "wasm32")]
            zoon::println!("  returning last_slot={:?}", last_slot);
            last_slot
        }
    }

    /// Compile a single expression to a reactive node.
    pub fn compile_expression(&mut self, expr: &Spanned<Expression<'code>>) -> SlotId {
        // Generate a SourceId from span info
        let source_id = SourceId {
            stable_id: expr.span.start as u64,
            parse_order: expr.span.start as u32,
        };

        match &expr.node {
            Expression::Literal(lit) => self.compile_literal(lit),

            Expression::Variable(var) => {
                // Variable definition: name: value
                let value_slot = self.compile_expression(&var.value);
                self.local_bindings.insert(var.name.to_string(), value_slot);
                value_slot
            }

            Expression::Object(obj) => self.compile_object_expr(obj),

            Expression::TaggedObject { tag, object } => {
                let tag_id = self.event_loop.arena.intern_tag(tag);
                let obj_slot = self.compile_object_expr(object);
                // Create tagged object wrapper
                let slot = self.event_loop.arena.alloc();
                if let Some(node) = self.event_loop.arena.get_mut(slot) {
                    let payload = Payload::TaggedObject {
                        tag: tag_id,
                        fields: obj_slot
                    };
                    node.set_kind(NodeKind::Producer {
                        value: Some(payload.clone())
                    });
                    // Also set current_value so it's accessible during compilation
                    node.extension_mut().current_value = Some(payload);
                }
                slot
            }

            Expression::List { items } => {
                let item_slots: Vec<_> = items.iter()
                    .map(|item| self.compile_expression(item))
                    .collect();
                self.compile_list_items(source_id, item_slots)
            }

            Expression::Latest { inputs } => {
                let input_slots: Vec<_> = inputs.iter()
                    .map(|input| self.compile_expression(input))
                    .collect();
                self.compile_latest(input_slots)
            }

            Expression::Hold { state_param, body } => {
                // We need the piped input (handled by Pipe compilation)
                // For now, just compile the body with state binding
                let body_slot = self.compile_expression(body);
                // Initial value comes from pipe input, not here
                self.compile_hold(Payload::Unit, Some(body_slot), None)
            }

            Expression::Then { body } => {
                let body_slot = self.compile_expression(body);
                // Input comes from pipe
                let input_slot = self.event_loop.arena.alloc();
                self.compile_then(input_slot, body_slot)
            }

            Expression::When { arms } => {
                self.compile_when_expr(arms)
            }

            Expression::While { arms } => {
                self.compile_while_expr(arms)
            }

            Expression::Pipe { from, to } => {
                #[cfg(target_arch = "wasm32")]
                {
                    let to_type = match &to.node {
                        Expression::FunctionCall { path, .. } => format!("FunctionCall({:?})", path),
                        Expression::Pipe { .. } => "Pipe".to_string(),
                        Expression::Hold { .. } => "Hold".to_string(),
                        Expression::Then { .. } => "Then".to_string(),
                        Expression::When { .. } => "When".to_string(),
                        Expression::While { .. } => "While".to_string(),
                        Expression::LinkSetter { .. } => "LinkSetter".to_string(),
                        Expression::FieldAccess { path } => format!("FieldAccess({:?})", path),
                        _ => format!("Unknown"),
                    };
                    zoon::println!("Expression::Pipe: to={}", to_type);
                }
                let from_slot = self.compile_expression(from);
                self.compile_pipe_to(from_slot, to)
            }

            Expression::Skip => {
                // SKIP produces no value - return an empty producer
                self.compile_constant(Payload::Unit)
            }

            Expression::Block { variables, output } => {
                // Compile block with local bindings
                let old_bindings = self.local_bindings.clone();

                for var in variables {
                    let value_slot = self.compile_expression(&var.node.value);
                    self.local_bindings.insert(var.node.name.to_string(), value_slot);
                }

                let output_slot = self.compile_expression(output);

                self.local_bindings = old_bindings;
                output_slot
            }

            Expression::Alias(alias) => {
                // Variable reference
                use crate::parser::Alias;
                match alias {
                    Alias::WithoutPassed { parts, .. } => {
                        if let Some(name) = parts.first() {
                            #[cfg(target_arch = "wasm32")]
                            if *name == "item" {
                                zoon::println!("Alias 'item': found in bindings? {} slot={:?}",
                                    self.local_bindings.contains_key(*name),
                                    self.local_bindings.get(*name).map(|s| s.index));
                            }

                            if let Some(&slot) = self.local_bindings.get(*name) {
                                // Handle field path: a.b.c
                                if parts.len() == 1 {
                                    // Simple variable reference - return slot directly
                                    slot
                                } else {
                                    // Field path needs Wire to track traversal
                                    let mut current = self.compile_wire(slot);
                                    for field_name in parts.iter().skip(1) {
                                        let field_id = self.event_loop.arena.intern_field(field_name);
                                        current = self.compile_field_access(current, field_id);
                                    }
                                    current
                                }
                            } else if parts.len() == 1 && name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
                                // Single PascalCase identifier not in bindings - treat as a tag
                                // This handles NoElement, All, Active, Completed, etc.
                                #[cfg(target_arch = "wasm32")]
                                zoon::println!("  treating '{}' as tag", name);
                                let tag_id = self.event_loop.arena.intern_tag(name);
                                self.compile_constant(Payload::Tag(tag_id))
                            } else {
                                // Unknown variable - return unit
                                #[cfg(target_arch = "wasm32")]
                                zoon::println!("  returning Unit for unknown variable '{}'", name);
                                self.compile_constant(Payload::Unit)
                            }
                        } else {
                            self.compile_constant(Payload::Unit)
                        }
                    }
                    Alias::WithPassed { extra_parts } => {
                        // PASSED reference
                        if let Some(passed_slot) = self.current_passed() {
                            let mut current = self.compile_wire(passed_slot);
                            for field_name in extra_parts {
                                let field_id = self.event_loop.arena.intern_field(field_name);
                                current = self.compile_field_access(current, field_id);
                            }
                            current
                        } else {
                            self.compile_constant(Payload::Unit)
                        }
                    }
                }
            }

            Expression::TextLiteral { parts } => {
                self.compile_text_literal(parts)
            }

            Expression::FunctionCall { path, arguments } => {
                self.compile_function_call(path, arguments)
            }

            Expression::Function { name, parameters, body } => {
                // Store function definition for later invocation
                let param_names: Vec<String> = parameters.iter()
                    .map(|p| p.node.to_string())
                    .collect();

                let func_def = FunctionDef {
                    name: name.to_string(),
                    parameters: param_names,
                    body: (**body).clone(),
                };

                self.functions.insert(name.to_string(), func_def);

                // Function definition itself produces no value
                self.compile_constant(Payload::Unit)
            }

            Expression::Comparator(cmp) => {
                self.compile_comparator(cmp)
            }

            Expression::ArithmeticOperator(op) => {
                self.compile_arithmetic(op)
            }

            Expression::Flush { value } => {
                let value_slot = self.compile_expression(value);
                // FLUSH wraps value in error handling
                value_slot
            }

            Expression::Spread { value } => {
                self.compile_expression(value)
            }

            Expression::FieldAccess { path } => {
                // Single-element path is a variable reference
                if path.len() == 1 {
                    let var_name = path[0];
                    #[cfg(target_arch = "wasm32")]
                    zoon::println!("FieldAccess single-elem '{}': looking in bindings (has {} entries)",
                        var_name, self.local_bindings.len());
                    if let Some(&slot) = self.local_bindings.get(var_name) {
                        #[cfg(target_arch = "wasm32")]
                        zoon::println!("  found slot {:?}", slot);
                        return slot;
                    } else {
                        #[cfg(target_arch = "wasm32")]
                        zoon::println!("  NOT FOUND, bindings: {:?}", self.local_bindings.keys().collect::<Vec<_>>());
                    }
                }
                // Multi-part field access needs a source (from pipe)
                // or unknown variable - return Unit
                self.compile_constant(Payload::Unit)
            }

            Expression::Link => {
                // LINK creates an IOPad that can receive external events (e.g., button presses)
                // For element event handlers like element: [event: [key_down: LINK]]
                self.compile_io_pad("event")
            }

            Expression::LinkSetter { alias } => {
                // LinkSetter assigns a value to a LINK
                // For now, just return unit
                self.compile_constant(Payload::Unit)
            }

            Expression::Map { entries } => {
                // Map entries
                self.compile_constant(Payload::Unit)
            }

            Expression::Bits { .. } | Expression::Memory { .. } | Expression::Bytes { .. } => {
                // Hardware types - not yet implemented
                self.compile_constant(Payload::Unit)
            }
        }
    }

    fn compile_literal(&mut self, lit: &Literal) -> SlotId {
        match lit {
            Literal::Number(n) => self.compile_constant(Payload::Number(*n)),
            Literal::Text(s) => self.compile_constant(Payload::Text(Arc::from(*s))),
            Literal::Tag(t) => {
                // Special case: True and False tags should become Bool payloads
                if *t == "True" {
                    return self.compile_constant(Payload::Bool(true));
                }
                if *t == "False" {
                    return self.compile_constant(Payload::Bool(false));
                }
                let tag_id = self.event_loop.arena.intern_tag(t);
                self.compile_constant(Payload::Tag(tag_id))
            }
        }
    }

    /// Pre-allocate Router and field placeholders for an object structure.
    /// This creates Wire placeholders for each field so they can be referenced before compilation.
    /// Returns the Router slot that holds all fields.
    fn pre_allocate_object_structure(&mut self, obj: &Object<'code>) -> SlotId {
        let router_slot = self.event_loop.arena.alloc();
        let mut fields = std::collections::HashMap::new();

        for var in &obj.variables {
            let field_id = self.event_loop.arena.intern_field(var.node.name);
            // Create a Wire placeholder for this field
            let field_slot = self.event_loop.arena.alloc();
            if let Some(node) = self.event_loop.arena.get_mut(field_slot) {
                node.set_kind(NodeKind::Wire { source: None });
            }
            fields.insert(field_id, field_slot);
            #[cfg(target_arch = "wasm32")]
            zoon::println!("  Pre-alloc field '{}' (id={}) -> slot {:?}", var.node.name, field_id, field_slot);
        }

        if let Some(node) = self.event_loop.arena.get_mut(router_slot) {
            node.set_kind(NodeKind::Router { fields });
        }

        router_slot
    }

    fn compile_object_expr(&mut self, obj: &Object<'code>) -> SlotId {
        // Pre-allocate the Router slot so inner expressions can reference it via current_variable_slot
        let router_slot = self.event_loop.arena.alloc();
        if let Some(node) = self.event_loop.arena.get_mut(router_slot) {
            node.set_kind(NodeKind::Router { fields: std::collections::HashMap::new() });
        }

        // If we're compiling a variable's value, set its current_value to this Router early
        // This allows nested expressions to find fields via store.field
        if let Some(var_slot) = self.current_variable_slot {
            if let Some(node) = self.event_loop.arena.get_mut(var_slot) {
                node.extension_mut().current_value = Some(Payload::ObjectHandle(router_slot));
            }
            #[cfg(target_arch = "wasm32")]
            zoon::println!("OBJECT: set var_slot {:?} current_value to ObjectHandle({:?})", var_slot, router_slot);
        }

        let mut fields = Vec::new();

        // Save old bindings to restore later
        let old_bindings = self.local_bindings.clone();

        for var in &obj.variables {
            let field_id = self.event_loop.arena.intern_field(var.node.name);
            let value_slot = self.compile_expression(&var.node.value);
            fields.push((field_id, value_slot));
            // Add to local_bindings so later fields can reference this one
            self.local_bindings.insert(var.node.name.to_string(), value_slot);

            #[cfg(target_arch = "wasm32")]
            if var.node.name == "completed" {
                zoon::println!("OBJECT_FIELD: router={:?} field='completed' -> slot={:?}", router_slot, value_slot);
            }

            // Also add field to the pre-allocated Router
            if let Some(node) = self.event_loop.arena.get_mut(router_slot) {
                if let Some(NodeKind::Router { fields: router_fields }) = node.kind_mut() {
                    router_fields.insert(field_id, value_slot);
                }
            }
        }

        // Restore old bindings
        self.local_bindings = old_bindings;

        // Store ObjectHandle in current_value for reads
        if let Some(node) = self.event_loop.arena.get_mut(router_slot) {
            node.extension_mut().current_value = Some(Payload::ObjectHandle(router_slot));
        }

        router_slot
    }

    fn compile_list_items(&mut self, source_id: SourceId, item_slots: Vec<SlotId>) -> SlotId {
        let slot = self.compile_list(source_id);

        // Add items to the bus
        if let Some(node) = self.event_loop.arena.get_mut(slot) {
            if let Some(NodeKind::Bus { items, alloc_site, static_item_count }) = node.kind_mut() {
                for item_slot in item_slots.iter() {
                    let key = alloc_site.allocate();
                    items.push((key, *item_slot));
                }
                // Mark all items added during compilation as static (not persisted)
                *static_item_count = items.len();
            }
        }

        slot
    }

    fn compile_pipe_to(&mut self, input_slot: SlotId, to: &Spanned<Expression<'code>>) -> SlotId {
        #[cfg(target_arch = "wasm32")]
        {
            let expr_type = match &to.node {
                Expression::Hold { .. } => "Hold",
                Expression::Then { .. } => "Then",
                Expression::When { .. } => "When",
                Expression::While { .. } => "While",
                Expression::FunctionCall { path, .. } => {
                    zoon::println!("compile_pipe_to: FunctionCall path={:?}", path);
                    "FunctionCall"
                }
                Expression::LinkSetter { .. } => "LinkSetter",
                _ => "Other",
            };
            zoon::println!("compile_pipe_to: expr_type={} input_slot={:?}", expr_type, input_slot);
        }

        match &to.node {
            Expression::Hold { state_param, body } => {
                // Debug: log all HOLD compilations
                let input_kind = self.event_loop.arena.get(input_slot).and_then(|n| n.kind().cloned());
                #[cfg(target_arch = "wasm32")]
                zoon::println!("HOLD_PIPE: input_slot={:?} input_kind={:?} current_var={:?}",
                    input_slot, input_kind, self.current_variable_slot);

                // Get initial value from input (may be Unit if input is a runtime-only slot)
                let initial = self.event_loop.get_current_value(input_slot)
                    .cloned()
                    .unwrap_or(Payload::Unit);

                // If initial value is Unit, we'll wire the input for runtime initialization
                let needs_initial_input = matches!(initial, Payload::Unit);
                let initial_input_slot = if needs_initial_input { Some(input_slot) } else { None };

                #[cfg(target_arch = "wasm32")]
                zoon::println!("HOLD_PIPE: initial={:?} needs_initial_input={}", initial, needs_initial_input);

                // Create the HOLD register first (without body connection)
                let hold_slot = self.compile_hold(initial.clone(), None, initial_input_slot);

                #[cfg(target_arch = "wasm32")]
                zoon::println!("HOLD_CREATED: hold_slot={:?} initial={:?}", hold_slot, initial);

                // Create a wire that represents the current state
                // This wire reads from the register's stored_value but does NOT subscribe to updates.
                // This is crucial: state changes inside HOLD must NOT trigger body re-evaluation.
                // The body should only re-evaluate from external triggers (THEN, timers, etc.)
                let state_wire = self.event_loop.arena.alloc();
                if let Some(node) = self.event_loop.arena.get_mut(state_wire) {
                    node.set_kind(NodeKind::Wire { source: Some(hold_slot) });
                }
                // NOTE: No routing from hold_slot to state_wire!
                // Body nodes can still READ state via get_current_value() which follows the Wire source.

                // Bind state_param to this wire
                let old_bindings = self.local_bindings.clone();
                self.local_bindings.insert(state_param.to_string(), state_wire);

                // If we're compiling a variable's value, set its current_value to the initial value
                // This allows code inside the HOLD body to find the initial value through the variable
                if let Some(var_slot) = self.current_variable_slot {
                    // Find the Router by following Wire chains
                    let obj_handle = self.find_router_slot(input_slot);
                    #[cfg(target_arch = "wasm32")]
                    zoon::println!("HOLD_VAR: var_slot={:?} input_slot={:?} -> router={:?}", var_slot, input_slot, obj_handle);

                    if let Some(obj_slot) = obj_handle {
                        // Set the variable Wire's current_value to an ObjectHandle pointing to initial value's object
                        // This enables find_bus_slot to traverse through the variable before it's fully wired
                        if let Some(node) = self.event_loop.arena.get_mut(var_slot) {
                            node.extension_mut().current_value = Some(Payload::ObjectHandle(obj_slot));
                        }
                        #[cfg(target_arch = "wasm32")]
                        zoon::println!("HOLD_VAR: SET var_slot {:?} -> ObjectHandle({:?})", var_slot, obj_slot);
                    }
                }

                // Now compile the body (which can reference `state`)
                let body_slot = self.compile_expression(body);

                // Restore bindings
                self.local_bindings = old_bindings;

                // Check if body is a pure self-reference (just `state`)
                // In that case, don't set body_input to prevent infinite loops
                let is_pure_self_reference = body_slot == state_wire;

                // Update HOLD to include body_input, keeping the initial value and initial_input
                if let Some(node) = self.event_loop.arena.get_mut(hold_slot) {
                    node.set_kind(NodeKind::Register {
                        stored_value: Some(initial),
                        body_input: if is_pure_self_reference { None } else { Some(body_slot) },
                        initial_input: initial_input_slot,
                        initial_received: !needs_initial_input,
                    });
                }

                // Wire body output to register input, but only if body is not just the state wire
                // (prevents infinite loop when body is just `state`)
                if !is_pure_self_reference {
                    self.event_loop.routing.add_route(body_slot, hold_slot, Port::Input(0));
                }

                hold_slot
            }

            Expression::Then { body } => {
                let body_slot = self.compile_expression(body);
                let then_slot = self.compile_then(input_slot, body_slot);
                then_slot
            }

            Expression::When { arms } => {
                // Compile arm bodies and patterns with bindings
                let mut compiled_arms = Vec::new();
                for arm in arms {
                    let pattern = self.convert_pattern(&arm.pattern);
                    // Add pattern bindings to local_bindings before compiling body
                    let old_bindings = self.local_bindings.clone();
                    self.add_pattern_bindings(&arm.pattern, input_slot);
                    let body_slot = self.compile_expression(&arm.body);
                    self.local_bindings = old_bindings;
                    compiled_arms.push((pattern, body_slot));
                }
                // Use input_slot from pipe directly
                self.compile_when(input_slot, compiled_arms)
            }

            Expression::While { arms } => {
                // Compile arm bodies and patterns with bindings
                let mut compiled_arms = Vec::new();
                for arm in arms {
                    let pattern = self.convert_pattern(&arm.pattern);
                    // Add pattern bindings to local_bindings before compiling body
                    let old_bindings = self.local_bindings.clone();
                    self.add_pattern_bindings(&arm.pattern, input_slot);
                    let body_slot = self.compile_expression(&arm.body);
                    self.local_bindings = old_bindings;
                    compiled_arms.push((pattern, body_slot));
                }
                // Use input_slot from pipe directly
                self.compile_while(input_slot, compiled_arms)
            }

            Expression::FieldAccess { path } => {
                // Chain field accesses
                let mut current = input_slot;
                for field_name in path {
                    let field_id = self.event_loop.arena.intern_field(field_name);
                    current = self.compile_field_access(current, field_id);
                }
                current
            }

            Expression::FunctionCall { path, arguments } => {
                // Function call with piped input
                self.compile_function_call_with_input(path, arguments, input_slot)
            }

            Expression::LinkSetter { alias } => {
                // LINK setter: connects the input element's IOPads to target LINKs
                // e.g., button |> LINK { store.filter_buttons.completed }
                // Connects ALL event types: press, click, key_down, change, etc.
                //
                // For templates (where the target path can't be resolved at compile time),
                // we create a LinkResolver node that will be processed at clone time.

                use crate::parser::Alias;

                let event_field_id = self.event_loop.arena.intern_field("event");

                // Result type: either fully resolved slot, or deferred resolution info
                enum LinkTarget {
                    Resolved(SlotId),
                    Deferred { source: SlotId, path: Vec<FieldId> },
                }

                // Get the target LINK from alias, tracking whether path was fully resolved
                let target = match &alias.node {
                    Alias::WithPassed { extra_parts } => {
                        if let Some(passed_slot) = self.current_passed() {
                            let mut current = passed_slot;
                            let mut resolved_count = 0;
                            let path_len = extra_parts.len();

                            for field_name in extra_parts {
                                let field_id = self.event_loop.arena.intern_field(field_name);
                                if let Some(field_slot) = self.get_field(current, field_id) {
                                    current = field_slot;
                                    resolved_count += 1;
                                } else {
                                    break;
                                }
                            }

                            if resolved_count == path_len {
                                Some(LinkTarget::Resolved(current))
                            } else {
                                // Path couldn't be fully resolved - defer to clone time
                                let path: Vec<FieldId> = extra_parts.iter()
                                    .map(|name| self.event_loop.arena.intern_field(name))
                                    .collect();
                                Some(LinkTarget::Deferred { source: passed_slot, path })
                            }
                        } else {
                            None
                        }
                    }
                    Alias::WithoutPassed { parts, .. } => {
                        if let Some(&first_slot) = parts.first().and_then(|name| self.local_bindings.get(*name)) {
                            let mut current = first_slot;
                            let mut resolved_count = 0;
                            let expected_parts = parts.len() - 1; // Skip first part (the variable name)

                            for field_name in parts.iter().skip(1) {
                                let field_id = self.event_loop.arena.intern_field(field_name);
                                if let Some(field_slot) = self.get_field(current, field_id) {
                                    current = field_slot;
                                    resolved_count += 1;
                                } else {
                                    break;
                                }
                            }

                            if resolved_count == expected_parts {
                                Some(LinkTarget::Resolved(current))
                            } else {
                                // Path couldn't be fully resolved (likely a template context)
                                // Create a LinkResolver to defer the connection to clone time
                                let path: Vec<FieldId> = parts.iter().skip(1)
                                    .map(|name| self.event_loop.arena.intern_field(name))
                                    .collect();
                                Some(LinkTarget::Deferred { source: first_slot, path })
                            }
                        } else {
                            None
                        }
                    }
                };

                #[cfg(target_arch = "wasm32")]
                zoon::println!("LinkSetter: input_slot={:?} target={:?}", input_slot,
                    match &target {
                        Some(LinkTarget::Resolved(s)) => format!("Resolved({:?})", s),
                        Some(LinkTarget::Deferred { source, path }) => format!("Deferred(source={:?}, path_len={})", source, path.len()),
                        None => "None".to_string(),
                    });

                // Handle the target based on whether it was fully resolved
                match target {
                    Some(LinkTarget::Resolved(target_slot)) => {
                        // Convert target LINK (IOPad) to a Wire pointing to the input element
                        // This makes field access like `target.event.key_down` follow through to the input element
                        if let Some(node) = self.event_loop.arena.get_mut(target_slot) {
                            #[cfg(target_arch = "wasm32")]
                            zoon::println!("LinkSetter: converting target {:?} to Wire -> input {:?}", target_slot, input_slot);
                            // Replace IOPad with Wire pointing to input element
                            node.set_kind(NodeKind::Wire { source: Some(input_slot) });
                        }
                    }
                    Some(LinkTarget::Deferred { source, path }) => {
                        // Can't resolve path at compile time (template context)
                        // Create a LinkResolver node that will be processed at clone time
                        // The resolver is connected to input_slot via routing so it gets cloned
                        #[cfg(target_arch = "wasm32")]
                        zoon::println!("LinkSetter: creating LinkResolver for deferred connection (source={:?}, path_len={})", source, path.len());

                        let resolver_slot = self.event_loop.arena.alloc();
                        if let Some(node) = self.event_loop.arena.get_mut(resolver_slot) {
                            node.set_kind(NodeKind::LinkResolver {
                                input_element: input_slot,
                                target_source: source,
                                target_path: path,
                            });
                        }
                        // Add route so traversal can find the resolver via routing table
                        self.event_loop.routing.add_route(input_slot, resolver_slot, Port::Input(0));

                        // Return input_slot (pass-through) - LINK is a side-effect, not a transform
                        // The resolver will be found during cloning via the routing table
                    }
                    None => {
                        #[cfg(target_arch = "wasm32")]
                        zoon::println!("LinkSetter: no target found, skipping");
                    }
                }

                // Also get input's element field if present (for Element types)
                let element_field_id = self.event_loop.arena.intern_field("element");
                let input_element_slot = self.get_field(input_slot, element_field_id);

                // Get input event router (from element.event or direct event field)
                let input_event_slot = input_element_slot
                    .and_then(|el| self.get_field(el, event_field_id))
                    .or_else(|| self.get_field(input_slot, event_field_id));

                #[cfg(target_arch = "wasm32")]
                zoon::println!("LinkSetter: input_element={:?} input_event={:?}", input_element_slot, input_event_slot);

                // Connect ALL event IOPads from input to routing table subscribers
                // This ensures events from the UI element reach the store's LINK readers
                if let Some(input_ev) = input_event_slot {
                    // Debug: check what kind of node input_ev is
                    #[cfg(target_arch = "wasm32")]
                    if let Some(node) = self.event_loop.arena.get(input_ev) {
                        zoon::println!("LinkSetter: input_event {:?} kind={:?}", input_ev, node.kind());
                    }

                    let event_types = ["press", "click", "key_down", "change"];
                    for event_type in event_types {
                        let field_id = self.event_loop.arena.intern_field(event_type);
                        let field_slot = self.get_field(input_ev, field_id);

                        #[cfg(target_arch = "wasm32")]
                        zoon::println!("LinkSetter: {} field_slot={:?}", event_type, field_slot);

                        let input_iopad = field_slot
                            .and_then(|slot| self.follow_wire_to_iopad(slot));

                        #[cfg(target_arch = "wasm32")]
                        if let Some(src) = input_iopad {
                            zoon::println!("LinkSetter: found {} IOPad {:?}", event_type, src);
                        }
                    }
                }

                // Pass through the input element
                input_slot
            }

            _ => {
                // Generic pipe: compile target and wire
                let target_slot = self.compile_expression(to);
                self.event_loop.routing.add_route(input_slot, target_slot, Port::Input(0));
                target_slot
            }
        }
    }

    fn compile_when_expr(&mut self, arms: &[Arm<'code>]) -> SlotId {
        let compiled_arms: Vec<_> = arms.iter()
            .map(|arm| {
                let pattern = self.convert_pattern(&arm.pattern);
                let body_slot = self.compile_expression(&arm.body);
                (pattern, body_slot)
            })
            .collect();

        // Create a placeholder input
        let input_slot = self.event_loop.arena.alloc();
        self.compile_when(input_slot, compiled_arms)
    }

    fn compile_while_expr(&mut self, arms: &[Arm<'code>]) -> SlotId {
        let compiled_arms: Vec<_> = arms.iter()
            .map(|arm| {
                let pattern = self.convert_pattern(&arm.pattern);
                let body_slot = self.compile_expression(&arm.body);
                (pattern, body_slot)
            })
            .collect();

        let input_slot = self.event_loop.arena.alloc();
        self.compile_while(input_slot, compiled_arms)
    }

    /// Convert an AST Pattern to a RuntimePattern for matching.
    fn convert_pattern(&mut self, pattern: &Pattern<'code>) -> RuntimePattern {
        match pattern {
            Pattern::Literal(lit) => {
                let payload = self.literal_to_payload(lit);
                RuntimePattern::Literal(payload)
            }
            Pattern::WildCard => RuntimePattern::Wildcard,
            Pattern::Alias { name } => {
                // Special case: True and False should match Payload::Bool
                if *name == "True" {
                    return RuntimePattern::Literal(Payload::Bool(true));
                }
                if *name == "False" {
                    return RuntimePattern::Literal(Payload::Bool(false));
                }
                // A variable binding - captures the value
                RuntimePattern::Binding(name.to_string())
            }
            Pattern::List { items } => {
                let sub_patterns = items.iter()
                    .map(|p| self.convert_pattern(p))
                    .collect();
                RuntimePattern::List(sub_patterns)
            }
            Pattern::Object { variables } => {
                let field_patterns = variables.iter()
                    .map(|var| {
                        let field_id = self.event_loop.arena.intern_field(var.name);
                        let sub_pattern = var.value.as_ref()
                            .map(|p| self.convert_pattern(p))
                            .unwrap_or(RuntimePattern::Wildcard);
                        (field_id, sub_pattern)
                    })
                    .collect();
                RuntimePattern::Object(field_patterns)
            }
            Pattern::TaggedObject { tag, variables } => {
                // Special case: True and False patterns should match Payload::Bool
                if *tag == "True" && variables.is_empty() {
                    return RuntimePattern::Literal(Payload::Bool(true));
                }
                if *tag == "False" && variables.is_empty() {
                    return RuntimePattern::Literal(Payload::Bool(false));
                }

                // Tagged object pattern - match by tag ID
                // If there are variables, we also check fields
                let tag_id = self.event_loop.arena.intern_tag(tag);
                if variables.is_empty() {
                    RuntimePattern::Tag(tag_id)
                } else {
                    // For now, just match the tag (full field matching TBD)
                    RuntimePattern::Tag(tag_id)
                }
            }
            Pattern::Map { entries } => {
                // Map patterns - treat as object patterns for now
                let field_patterns = entries.iter()
                    .filter_map(|entry| {
                        // Convert key to field ID if possible
                        if let Pattern::Literal(Literal::Text(s)) = &entry.key {
                            let field_id = self.event_loop.arena.intern_field(s);
                            let value_pattern = entry.value.as_ref()
                                .map(|v| self.convert_pattern(v))
                                .unwrap_or(RuntimePattern::Wildcard);
                            Some((field_id, value_pattern))
                        } else {
                            None
                        }
                    })
                    .collect();
                RuntimePattern::Object(field_patterns)
            }
        }
    }

    /// Add pattern bindings to local_bindings.
    /// For simple variable patterns like `x => body`, binds x to the input slot.
    fn add_pattern_bindings(&mut self, pattern: &Pattern<'code>, input_slot: SlotId) {
        match pattern {
            Pattern::Alias { name } => {
                // Skip True/False - they're not bindings
                if *name != "True" && *name != "False" {
                    self.local_bindings.insert(name.to_string(), input_slot);
                }
            }
            // TODO: Handle nested patterns (List, Object, etc.) if needed
            _ => {}
        }
    }

    /// Convert a Literal to a Payload for pattern matching.
    fn literal_to_payload(&mut self, lit: &Literal<'code>) -> Payload {
        match lit {
            Literal::Number(n) => Payload::Number(*n),
            Literal::Tag(s) => {
                // Special case: True and False tags should become Bool payloads
                if *s == "True" {
                    return Payload::Bool(true);
                }
                if *s == "False" {
                    return Payload::Bool(false);
                }
                let tag_id = self.event_loop.arena.intern_tag(s);
                Payload::Tag(tag_id)
            }
            Literal::Text(s) => Payload::Text(Arc::from(*s)),
        }
    }

    fn compile_text_literal(&mut self, parts: &[TextPart]) -> SlotId {
        let mut template = String::new();
        let mut deps = Vec::new();

        for part in parts {
            match part {
                TextPart::Text(s) => template.push_str(s),
                TextPart::Interpolation { var, .. } => {
                    // Use numbered placeholder {0}, {1}, etc.
                    template.push_str(&format!("{{{}}}", deps.len()));
                    // Resolve the variable and add as dependency
                    if let Some(&slot) = self.local_bindings.get(*var) {
                        deps.push(self.compile_wire(slot));
                    } else {
                        // Variable not found - add a Unit placeholder to keep indexing correct
                        deps.push(self.compile_constant(Payload::Unit));
                    }
                }
            }
        }

        self.compile_text_template(template, deps)
    }

    fn compile_comparator(&mut self, cmp: &crate::parser::Comparator<'code>) -> SlotId {
        use crate::parser::Comparator;
        use crate::engine_v2::node::ComparisonOp;

        match cmp {
            Comparator::Equal { operand_a, operand_b } => {
                let a = self.compile_expression(operand_a);
                let b = self.compile_expression(operand_b);
                self.compile_comparison_node(a, b, ComparisonOp::Equal)
            }
            Comparator::NotEqual { operand_a, operand_b } => {
                let a = self.compile_expression(operand_a);
                let b = self.compile_expression(operand_b);
                self.compile_comparison_node(a, b, ComparisonOp::NotEqual)
            }
            Comparator::Greater { operand_a, operand_b } => {
                let a = self.compile_expression(operand_a);
                let b = self.compile_expression(operand_b);
                self.compile_comparison_node(a, b, ComparisonOp::Greater)
            }
            Comparator::GreaterOrEqual { operand_a, operand_b } => {
                let a = self.compile_expression(operand_a);
                let b = self.compile_expression(operand_b);
                self.compile_comparison_node(a, b, ComparisonOp::GreaterOrEqual)
            }
            Comparator::Less { operand_a, operand_b } => {
                let a = self.compile_expression(operand_a);
                let b = self.compile_expression(operand_b);
                self.compile_comparison_node(a, b, ComparisonOp::Less)
            }
            Comparator::LessOrEqual { operand_a, operand_b } => {
                let a = self.compile_expression(operand_a);
                let b = self.compile_expression(operand_b);
                self.compile_comparison_node(a, b, ComparisonOp::LessOrEqual)
            }
        }
    }

    /// Compile a reactive comparison node between two slots.
    fn compile_comparison_node(&mut self, a: SlotId, b: SlotId, op: ComparisonOp) -> SlotId {
        // Get initial values from both slots (may be None if reactive)
        let val_a = self.get_payload_value(a);
        let val_b = self.get_payload_value(b);

        let slot = self.event_loop.arena.alloc();
        if let Some(node) = self.event_loop.arena.get_mut(slot) {
            node.set_kind(NodeKind::Comparison {
                op,
                left: Some(a),
                right: Some(b),
                left_value: val_a,
                right_value: val_b,
            });
        }

        // Subscribe to both inputs
        self.event_loop.routing.add_route(a, slot, Port::Input(0));
        self.event_loop.routing.add_route(b, slot, Port::Input(1));

        // Trigger initial computation if we have both values
        self.event_loop.mark_dirty(slot, Port::Input(0));

        slot
    }

    /// Helper: Get payload value from a slot.
    fn get_payload_value(&self, slot: SlotId) -> Option<Payload> {
        if let Some(node) = self.event_loop.arena.get(slot) {
            match node.kind() {
                Some(NodeKind::Producer { value }) => value.clone(),
                Some(NodeKind::Wire { source: Some(source) }) => self.get_payload_value(*source),
                _ => None,
            }
        } else {
            None
        }
    }

    /// Helper: Get number value from a slot.
    fn get_number_value(&self, slot: SlotId) -> Option<f64> {
        if let Some(node) = self.event_loop.arena.get(slot) {
            match node.kind() {
                Some(NodeKind::Producer { value: Some(Payload::Number(n)) }) => Some(*n),
                Some(NodeKind::Wire { source: Some(source) }) => self.get_number_value(*source),
                _ => None,
            }
        } else {
            None
        }
    }

    /// Compile a function call without piped input.
    fn compile_function_call(&mut self, path: &[&str], arguments: &[Spanned<Argument<'code>>]) -> SlotId {
        // Match known function paths
        let path_str: Vec<&str> = path.iter().copied().collect();

        #[cfg(target_arch = "wasm32")]
        if path_str.len() >= 2 && (path_str[0] == "List" || path_str[0] == "Router" || path_str[0] == "Stream" || path_str[0] == "Element") {
            zoon::println!("compile_function_call (non-piped): path={:?}", path_str);
        }
        #[cfg(target_arch = "wasm32")]
        if path_str.as_slice() == ["Element", "checkbox"] {
            zoon::println!(">>> Element/checkbox compile_function_call CALLED!");
        }

        match path_str.as_slice() {
            ["Timer", "interval"] => {
                // Timer/interval() - needs a Duration input
                // But if called without pipe, we just create a timer with default
                self.compile_timer(1000.0) // Default 1 second
            }
            ["Math", "sum"] => {
                // Math/sum() without input - just an accumulator starting at 0
                // Create a placeholder input
                let input = self.compile_constant(Payload::Number(0.0));
                self.compile_accumulator(input)
            }
            ["Document", "new"] => {
                // Document/new() - for now just pass through the first argument's value
                if let Some(arg) = arguments.first() {
                    if let Some(ref value) = arg.node.value {
                        // Named argument: Document/new(element: some_expr)
                        self.compile_expression(value)
                    } else {
                        // Positional argument: Document/new(result)
                        // The name IS the variable reference
                        let name = arg.node.name;
                        if let Some(&slot) = self.local_bindings.get(name) {
                            self.compile_wire(slot)
                        } else {
                            #[cfg(target_arch = "wasm32")]
                            zoon::println!("Document/new: unknown variable '{}'", name);
                            self.compile_constant(Payload::Unit)
                        }
                    }
                } else {
                    self.compile_constant(Payload::Unit)
                }
            }
            ["Element", "stripe"] => {
                self.compile_element_stripe(arguments)
            }
            ["Element", "button"] => {
                self.compile_element_button(arguments)
            }
            ["Element", "label"] => {
                self.compile_element_label(arguments)
            }
            ["Element", "container"] => {
                self.compile_element_container(arguments)
            }
            ["Element", "text_input"] => {
                self.compile_element_text_input(arguments)
            }
            ["Element", "checkbox"] => {
                self.compile_element_checkbox(arguments)
            }
            ["Element", "paragraph"] => {
                self.compile_element_paragraph(arguments)
            }
            ["Element", "link"] => {
                self.compile_element_link(arguments)
            }
            ["Element", "stack"] => {
                self.compile_element_stack(arguments)
            }
            // Text operations (without pipe)
            ["Text", "empty"] => {
                self.compile_constant(Payload::Text(Arc::from("")))
            }
            ["Text", "space"] => {
                self.compile_constant(Payload::Text(Arc::from(" ")))
            }
            // Router operations
            ["Router", "route"] => {
                // Return the global route slot, creating it if needed
                self.get_or_create_route_slot()
            }
            _ => {
                // Check if this is a user-defined function call (single-part path)
                if path_str.len() == 1 {
                    let func_name = path_str[0];
                    #[cfg(target_arch = "wasm32")]
                    zoon::println!("compile_function_call: looking up '{}' in {} functions", func_name, self.functions.len());
                    if let Some(func_def) = self.functions.get(func_name).cloned() {
                        #[cfg(target_arch = "wasm32")]
                        zoon::println!("compile_function_call: FOUND user function '{}'", func_name);
                        // Check for PASS: argument
                        let pass_value = arguments.iter()
                            .find(|arg| arg.node.name == "PASS")
                            .and_then(|arg| arg.node.value.as_ref())
                            .map(|value| self.compile_expression(value));

                        // No pipe input for non-piped function calls
                        return self.compile_user_function_call(&func_def, arguments, pass_value, None);
                    } else {
                        #[cfg(target_arch = "wasm32")]
                        zoon::println!("compile_function_call: '{}' NOT FOUND, available: {:?}", func_name, self.functions.keys().collect::<Vec<_>>());
                        // Make the missing function visible at runtime
                        #[cfg(target_arch = "wasm32")]
                        zoon::eprintln!("ERROR: Function '{}' not found!", func_name);
                    }
                }
                // Unknown function - return unit
                self.compile_constant(Payload::Unit)
            }
        }
    }

    /// Compile a user-defined function call.
    fn compile_user_function_call(
        &mut self,
        func_def: &FunctionDef<'code>,
        arguments: &[Spanned<Argument<'code>>],
        pass_value: Option<SlotId>,
        pipe_input: Option<SlotId>,
    ) -> SlotId {
        // Save old bindings
        let old_bindings = self.local_bindings.clone();

        // Bind parameters to argument values
        for (i, param_name) in func_def.parameters.iter().enumerate() {
            // First, try to use pipe_input for the first parameter
            let arg_slot = if i == 0 && pipe_input.is_some() {
                // Check if this parameter has an explicit argument
                let explicit_arg = arguments.iter()
                    .find(|arg| arg.node.name == param_name)
                    .and_then(|arg| arg.node.value.as_ref());

                if explicit_arg.is_some() {
                    // Explicit argument takes precedence
                    explicit_arg.map(|value| self.compile_expression(value))
                        .unwrap_or_else(|| self.compile_constant(Payload::Unit))
                } else {
                    // Use pipe input for first parameter
                    pipe_input.unwrap()
                }
            } else {
                // Find the argument with matching name
                let arg = arguments.iter()
                    .find(|arg| arg.node.name == param_name);

                if let Some(arg) = arg {
                    if let Some(ref value) = arg.node.value {
                        // Named argument with explicit value: Foo(x: some_expr)
                        self.compile_expression(value)
                    } else {
                        // Positional argument: Foo(x) where x is a variable reference
                        // The argument name IS the variable to look up
                        #[cfg(target_arch = "wasm32")]
                        zoon::println!("compile_user_function_call: positional arg '{}' looking up in bindings", arg.node.name);
                        if let Some(&slot) = self.local_bindings.get(arg.node.name) {
                            #[cfg(target_arch = "wasm32")]
                            zoon::println!("  found slot {:?}", slot);
                            slot
                        } else {
                            #[cfg(target_arch = "wasm32")]
                            zoon::println!("  NOT FOUND - returning Unit");
                            self.compile_constant(Payload::Unit)
                        }
                    }
                } else {
                    // No argument found for this parameter
                    self.compile_constant(Payload::Unit)
                }
            };

            // Only log for template-related calls (todo_item)
            #[cfg(target_arch = "wasm32")]
            if func_def.name == "todo_item" || func_def.name == "todo_checkbox" {
                zoon::println!("compile_user_function_call '{}': param '{}' bound to slot {:?}", func_def.name, param_name, arg_slot);
            }

            self.local_bindings.insert(param_name.clone(), arg_slot);
        }

        // Handle PASS if provided
        if let Some(pass_slot) = pass_value {
            self.push_pass(pass_slot);
        }

        // Compile the function body
        let body_result_slot = self.compile_expression(&func_def.body);

        // Pop PASS if we pushed it
        if pass_value.is_some() {
            self.pop_pass();
        }

        // Restore old bindings
        self.local_bindings = old_bindings;

        // If there's a pipe input, wrap in a Transformer so the result is emitted
        // each time the input fires. This is crucial for List/append where we need
        // new items created each time the input emits.
        if let Some(input_slot) = pipe_input {
            #[cfg(target_arch = "wasm32")]
            zoon::println!("compile_user_function_call: wrapping in Transformer, input={:?} body={:?}",
                input_slot, body_result_slot);
            self.compile_then(input_slot, body_result_slot)
        } else {
            body_result_slot
        }
    }

    /// Compile a function call with piped input.
    fn compile_function_call_with_input(
        &mut self,
        path: &[&str],
        arguments: &[Spanned<Argument<'code>>],
        input_slot: SlotId,
    ) -> SlotId {
        let path_str: Vec<&str> = path.iter().copied().collect();

        #[cfg(target_arch = "wasm32")]
        zoon::println!("compile_function_call_with_input: path={:?} input_slot={:?}", path_str, input_slot);

        match path_str.as_slice() {
            ["Timer", "interval"] => {
                // Timer/interval() with Duration input
                // Extract interval from input (Duration[seconds: X] or Duration[ms: X])
                let interval_ms = self.extract_duration_ms(input_slot);
                self.compile_timer(interval_ms)
            }
            ["Math", "sum"] => {
                // Math/sum() accumulates piped values
                self.compile_accumulator(input_slot)
            }
            ["Stream", "pulses"] => {
                // Stream/pulses() - emit 0, 1, 2, ..., N-1 where N is the piped count
                // First get the count value from the input
                let count = self.event_loop.get_current_value(input_slot)
                    .and_then(|v| match v {
                        Payload::Number(n) => Some(*n as u32),
                        _ => None,
                    })
                    .unwrap_or(0);
                self.compile_pulses(count)
            }
            ["Stream", "skip"] => {
                // Stream/skip(count: N) - skip first N values from source, then pass through
                let count = arguments.iter()
                    .find(|arg| arg.node.name == "count")
                    .and_then(|arg| arg.node.value.as_ref())
                    .and_then(|value_expr| {
                        let count_slot = self.compile_expression(value_expr);
                        self.event_loop.get_current_value(count_slot)
                            .and_then(|v| match v {
                                Payload::Number(n) => Some(*n as u32),
                                _ => None,
                            })
                    })
                    .unwrap_or(0);
                self.compile_skip(input_slot, count)
            }
            ["Document", "new"] => {
                // Document/new() - wrap input in a Document TaggedObject
                // This allows the bridge to detect it's a Document and use ReactiveEventLoop
                self.compile_document(input_slot)
            }

            // List operations
            ["List", "count"] => {
                self.compile_list_count(input_slot)
            }
            ["List", "is_empty"] => {
                self.compile_list_is_empty(input_slot)
            }
            ["List", "append"] => {
                // List/append(item: expr)
                // Pass raw expression for template instantiation (like List/map does)
                let item_expr = arguments.iter()
                    .find(|arg| arg.node.name == "item")
                    .and_then(|arg| arg.node.value.as_ref());
                self.compile_list_append(input_slot, item_expr)
            }
            ["List", "clear"] => {
                // List/clear(on: trigger_event)
                let trigger_slot = arguments.iter()
                    .find(|arg| arg.node.name == "on")
                    .and_then(|arg| arg.node.value.as_ref())
                    .map(|value| self.compile_expression(value));
                self.compile_list_clear(input_slot, trigger_slot)
            }
            ["List", "remove"] => {
                // List/remove(item, on: event)
                // Get the "on" argument - DON'T compile yet, it uses 'item' binding
                let on_expr = arguments.iter()
                    .find(|arg| arg.node.name == "on")
                    .and_then(|arg| arg.node.value.as_ref());
                // Get the item parameter name (first positional arg)
                let item_param = arguments.iter()
                    .find(|arg| arg.node.value.is_none())
                    .map(|arg| arg.node.name)
                    .unwrap_or("item");
                self.compile_list_remove(input_slot, item_param, on_expr)
            }
            ["List", "retain"] => {
                // List/retain(item, if: condition)
                // Get the "if" argument - this should reference 'item'
                let if_expr = arguments.iter()
                    .find(|arg| arg.node.name == "if")
                    .and_then(|arg| arg.node.value.as_ref());
                // Get the item parameter name (first positional arg)
                let item_param = arguments.iter()
                    .find(|arg| arg.node.value.is_none())
                    .map(|arg| arg.node.name)
                    .unwrap_or("item");
                self.compile_list_retain(input_slot, item_param, if_expr)
            }
            ["List", "map"] => {
                // List/map(item, new: transform_expr)
                // Get the "new" argument
                let new_expr = arguments.iter()
                    .find(|arg| arg.node.name == "new")
                    .and_then(|arg| arg.node.value.as_ref());
                // Get the item parameter name (first positional arg)
                let item_param = arguments.iter()
                    .find(|arg| arg.node.value.is_none())
                    .map(|arg| arg.node.name)
                    .unwrap_or("item");
                self.compile_list_map(input_slot, item_param, new_expr)
            }

            // Text operations
            ["Text", "trim"] => {
                self.compile_text_trim(input_slot)
            }
            ["Text", "is_not_empty"] => {
                self.compile_text_is_not_empty(input_slot)
            }

            // Bool operations
            ["Bool", "not"] => {
                self.compile_bool_not(input_slot)
            }
            ["Bool", "or"] => {
                // Bool/or(that: other_bool)
                let that_slot = arguments.iter()
                    .find(|arg| arg.node.name == "that")
                    .and_then(|arg| arg.node.value.as_ref())
                    .map(|value| self.compile_expression(value))
                    .unwrap_or_else(|| self.compile_constant(Payload::Bool(false)));
                self.compile_bool_or(input_slot, that_slot)
            }

            // Router operations
            ["Router", "go_to"] => {
                // Router/go_to() - update the global route when input arrives
                self.compile_router_go_to(input_slot)
            }

            _ => {
                // Check if this is a user-defined function call (single-part path)
                #[cfg(target_arch = "wasm32")]
                zoon::println!("compile_function_call_with_input FALLBACK: path={:?} functions={:?}",
                    path, self.functions.keys().collect::<Vec<_>>());

                if path.len() == 1 {
                    let func_name = path[0];
                    #[cfg(target_arch = "wasm32")]
                    zoon::println!("compile_function_call_with_input: checking for user function '{}'", func_name);

                    if let Some(func_def) = self.functions.get(func_name).cloned() {
                        #[cfg(target_arch = "wasm32")]
                        zoon::println!("compile_function_call_with_input: FOUND user-defined function '{}' with input {:?}", func_name, input_slot);

                        // Check for PASS: argument
                        let pass_value = arguments.iter()
                            .find(|arg| arg.node.name == "PASS")
                            .and_then(|arg| arg.node.value.as_ref())
                            .map(|value| self.compile_expression(value));

                        // Pass input_slot as pipe input for first positional argument
                        let result = self.compile_user_function_call(&func_def, arguments, pass_value, Some(input_slot));
                        #[cfg(target_arch = "wasm32")]
                        zoon::println!("compile_function_call_with_input: user function '{}' returned slot {:?}", func_name, result);
                        return result;
                    } else {
                        #[cfg(target_arch = "wasm32")]
                        zoon::println!("compile_function_call_with_input: '{}' NOT FOUND in functions", func_name);
                    }
                }
                // Unknown function - pass through input
                #[cfg(target_arch = "wasm32")]
                zoon::println!("compile_function_call_with_input: falling back to wire for {:?}", path);
                self.compile_wire(input_slot)
            }
        }
    }

    /// Extract duration in milliseconds from a Duration slot.
    fn extract_duration_ms(&self, slot: SlotId) -> f64 {
        // Try to get the value directly
        if let Some(node) = self.event_loop.arena.get(slot) {

            if let Some(kind) = node.kind() {
                match kind {
                    NodeKind::Producer { value: Some(Payload::TaggedObject { tag, fields }) } => {
                        // Get the tag name
                        if let Some(tag_name) = self.event_loop.arena.get_tag_name(*tag) {
                            #[cfg(target_arch = "wasm32")]
                            zoon::println!("  TaggedObject tag={}", tag_name);
                            if tag_name.as_ref() == "Duration" {
                                // Get the fields Router and extract seconds or ms
                                let ms = self.extract_duration_from_fields(*fields);
                                #[cfg(target_arch = "wasm32")]
                                zoon::println!("  extracted duration: {}ms", ms);
                                return ms;
                            }
                        }
                    }
                    NodeKind::Producer { value: Some(Payload::Number(n)) } => {
                        // Treat raw number as milliseconds
                        #[cfg(target_arch = "wasm32")]
                        zoon::println!("  raw number: {}ms", n);
                        return *n;
                    }
                    _ => {
                        #[cfg(target_arch = "wasm32")]
                        zoon::println!("  unhandled kind");
                    }
                }
            }
        }
        #[cfg(target_arch = "wasm32")]
        zoon::println!("  using default 1000ms");
        1000.0 // Default 1 second
    }

    /// Extract milliseconds from Duration fields.
    fn extract_duration_from_fields(&self, fields_slot: SlotId) -> f64 {
        if let Some(node) = self.event_loop.arena.get(fields_slot) {
            if let Some(NodeKind::Router { fields }) = node.kind() {
                // Try "seconds" field
                let seconds_id = self.event_loop.arena.get_field_id("seconds");
                if let Some(sec_id) = seconds_id {
                    if let Some(&field_slot) = fields.get(&sec_id) {
                        if let Some(payload) = self.event_loop.get_current_value(field_slot) {
                            if let Payload::Number(s) = payload {
                                return *s * 1000.0;
                            }
                        }
                        // Try from Producer directly
                        if let Some(field_node) = self.event_loop.arena.get(field_slot) {
                            if let Some(NodeKind::Producer { value: Some(Payload::Number(s)) }) = field_node.kind() {
                                return *s * 1000.0;
                            }
                        }
                    }
                }
                // Try "milliseconds" field (full name)
                let ms_id = self.event_loop.arena.get_field_id("milliseconds");
                if let Some(ms_id) = ms_id {
                    if let Some(&field_slot) = fields.get(&ms_id) {
                        if let Some(payload) = self.event_loop.get_current_value(field_slot) {
                            if let Payload::Number(ms) = payload {
                                return *ms;
                            }
                        }
                        if let Some(field_node) = self.event_loop.arena.get(field_slot) {
                            if let Some(NodeKind::Producer { value: Some(Payload::Number(ms)) }) = field_node.kind() {
                                return *ms;
                            }
                        }
                    }
                }
                // Try "ms" field (short name)
                let ms_id = self.event_loop.arena.get_field_id("ms");
                if let Some(ms_id) = ms_id {
                    if let Some(&field_slot) = fields.get(&ms_id) {
                        if let Some(payload) = self.event_loop.get_current_value(field_slot) {
                            if let Payload::Number(ms) = payload {
                                return *ms;
                            }
                        }
                        if let Some(field_node) = self.event_loop.arena.get(field_slot) {
                            if let Some(NodeKind::Producer { value: Some(Payload::Number(ms)) }) = field_node.kind() {
                                return *ms;
                            }
                        }
                    }
                }
            }
        }
        1000.0 // Default
    }

    fn compile_arithmetic(&mut self, op: &crate::parser::ArithmeticOperator<'code>) -> SlotId {
        use crate::parser::ArithmeticOperator;
        use crate::engine_v2::node::ArithmeticOp;

        match op {
            ArithmeticOperator::Add { operand_a, operand_b } => {
                let a = self.compile_expression(operand_a);
                let b = self.compile_expression(operand_b);
                self.compile_arithmetic_node(ArithmeticOp::Add, a, b)
            }
            ArithmeticOperator::Subtract { operand_a, operand_b } => {
                let a = self.compile_expression(operand_a);
                let b = self.compile_expression(operand_b);
                self.compile_arithmetic_node(ArithmeticOp::Subtract, a, b)
            }
            ArithmeticOperator::Multiply { operand_a, operand_b } => {
                let a = self.compile_expression(operand_a);
                let b = self.compile_expression(operand_b);
                self.compile_arithmetic_node(ArithmeticOp::Multiply, a, b)
            }
            ArithmeticOperator::Divide { operand_a, operand_b } => {
                let a = self.compile_expression(operand_a);
                let b = self.compile_expression(operand_b);
                self.compile_arithmetic_node(ArithmeticOp::Divide, a, b)
            }
            ArithmeticOperator::Negate { operand } => {
                let a = self.compile_expression(operand);
                self.compile_arithmetic_node(ArithmeticOp::Negate, a, a) // Only left is used
            }
        }
    }

    /// Compile an arithmetic operation node.
    fn compile_arithmetic_node(
        &mut self,
        op: crate::engine_v2::node::ArithmeticOp,
        left: SlotId,
        right: SlotId,
    ) -> SlotId {
        use crate::engine_v2::node::ArithmeticOp;

        // Get initial values at compile time
        let left_value = self.event_loop.get_current_value(left)
            .and_then(|p| match p { Payload::Number(n) => Some(*n), _ => None });
        let right_value = self.event_loop.get_current_value(right)
            .and_then(|p| match p { Payload::Number(n) => Some(*n), _ => None });

        let slot = self.event_loop.arena.alloc();

        // Compute initial result if both values are available
        let initial_result = match (&op, left_value, right_value) {
            (ArithmeticOp::Negate, Some(l), _) => Some(Payload::Number(-l)),
            (ArithmeticOp::Add, Some(l), Some(r)) => Some(Payload::Number(l + r)),
            (ArithmeticOp::Subtract, Some(l), Some(r)) => Some(Payload::Number(l - r)),
            (ArithmeticOp::Multiply, Some(l), Some(r)) => Some(Payload::Number(l * r)),
            (ArithmeticOp::Divide, Some(l), Some(r)) if r != 0.0 => Some(Payload::Number(l / r)),
            _ => None,
        };

        if let Some(node) = self.event_loop.arena.get_mut(slot) {
            node.set_kind(NodeKind::Arithmetic {
                op,
                left: Some(left),
                right: Some(right),
                left_value,
                right_value,
            });
            // Set initial result for render-time reads
            if let Some(result) = initial_result {
                node.extension_mut().current_value = Some(result);
            }
        }

        // Wire inputs
        self.event_loop.routing.add_route(left, slot, Port::Input(0));
        if !matches!(op, ArithmeticOp::Negate) {
            self.event_loop.routing.add_route(right, slot, Port::Input(1));
        }

        slot
    }

    // ============================================================
    // Phase 7c: LINK / IOPad
    // ============================================================

    /// Compile a LINK as an IOPad.
    /// For element event handlers like [event: [key_down: LINK, change: LINK]]
    fn compile_io_pad(&mut self, event_type: &str) -> SlotId {
        // Create the IOPad for events
        let iopad = self.event_loop.arena.alloc();
        if let Some(node) = self.event_loop.arena.get_mut(iopad) {
            node.set_kind(NodeKind::IOPad {
                element_slot: None,
                event_type: event_type.to_string(),
                connected: false,
            });
        }
        iopad
    }

    // ============================================================
    // Phase 7b: Element API
    // ============================================================

    /// Compile Document/new - wraps input in a Document TaggedObject.
    /// This creates a reactive document that the bridge can render.
    /// The bridge reads the root_element field at render time, so we don't need
    /// to subscribe to changes - the TaggedObject fields point to the live slots.
    fn compile_document(&mut self, root_element_slot: SlotId) -> SlotId {
        // Create "root_element" field
        let root_element_field_id = self.event_loop.arena.intern_field("root_element");

        // Create the fields Router - this points to root_element_slot directly,
        // so reading the field at render time will get the current value
        let fields_slot = self.compile_object(vec![
            (root_element_field_id, root_element_slot),
        ]);

        // Create the Document tag
        let tag_id = self.event_loop.arena.intern_tag("Document");

        // Create a Producer with the TaggedObject payload
        let slot = self.event_loop.arena.alloc();
        if let Some(node) = self.event_loop.arena.get_mut(slot) {
            let payload = Payload::TaggedObject { tag: tag_id, fields: fields_slot };
            node.set_kind(NodeKind::Producer {
                value: Some(payload.clone()),
            });
            node.extension_mut().current_value = Some(payload);
        }

        // Note: We don't subscribe to root_element_slot because:
        // 1. The TaggedObject fields already point to the live slot
        // 2. The bridge re-reads values on each render (triggered by version changes)
        // 3. Subscribing would corrupt the TaggedObject (Producer would overwrite with raw payload)

        slot
    }

    /// Compile Element/stripe function call.
    /// Element/stripe(element, direction, gap, style, items) -> ElementStripe[settings: [...]]
    fn compile_element_stripe(&mut self, arguments: &[Spanned<Argument<'code>>]) -> SlotId {
        // First compile `element` and add to local_bindings so other args can reference it
        let element_slot = self.get_argument_slot(arguments, "element");

        // Save old binding and add element to local_bindings
        let old_element_binding = self.local_bindings.insert("element".into(), element_slot);

        // Now compile other arguments that may reference element.hovered, etc.
        let direction_slot = self.get_argument_slot(arguments, "direction");
        let gap_slot = self.get_argument_slot(arguments, "gap");
        let style_slot = self.get_argument_slot(arguments, "style");
        let items_slot = self.get_argument_slot(arguments, "items");

        // Restore old binding
        if let Some(old) = old_element_binding {
            self.local_bindings.insert("element".into(), old);
        } else {
            self.local_bindings.remove("element");
        }

        // Create settings object with all the fields
        let element_field_id = self.event_loop.arena.intern_field("element");
        let direction_field_id = self.event_loop.arena.intern_field("direction");
        let gap_field_id = self.event_loop.arena.intern_field("gap");
        let style_field_id = self.event_loop.arena.intern_field("style");
        let items_field_id = self.event_loop.arena.intern_field("items");

        let settings_slot = self.compile_object(vec![
            (element_field_id, element_slot),
            (direction_field_id, direction_slot),
            (gap_field_id, gap_slot),
            (style_field_id, style_slot),
            (items_field_id, items_slot),
        ]);

        // Create the tagged object: ElementStripe[settings: ...]
        let tag_id = self.event_loop.arena.intern_tag("ElementStripe");
        let settings_field_id = self.event_loop.arena.intern_field("settings");

        let wrapper_slot = self.compile_object(vec![
            (settings_field_id, settings_slot),
        ]);

        // Create a producer with TaggedObject payload
        let slot = self.event_loop.arena.alloc();
        if let Some(node) = self.event_loop.arena.get_mut(slot) {
            node.set_kind(NodeKind::Producer {
                value: Some(Payload::TaggedObject {
                    tag: tag_id,
                    fields: wrapper_slot,
                }),
            });
        }

        slot
    }

    /// Compile Element/button function call.
    /// Element/button(element, style, label) -> ElementButton[element, event, settings: [...]]
    ///
    /// The `event` field is extracted from `element.event` and exposed at the top level
    /// to allow `button.event.press` access pattern (instead of `button.element.event.press`).
    fn compile_element_button(&mut self, arguments: &[Spanned<Argument<'code>>]) -> SlotId {
        // First compile `element` and add to local_bindings so other args can reference it
        let element_slot = self.get_argument_slot(arguments, "element");

        // Save old binding and add element to local_bindings
        let old_element_binding = self.local_bindings.insert("element".into(), element_slot);

        // Now compile other arguments that may reference element.hovered, etc.
        let style_slot = self.get_argument_slot(arguments, "style");
        let label_slot = self.get_argument_slot(arguments, "label");

        // Restore old binding
        if let Some(old) = old_element_binding {
            self.local_bindings.insert("element".into(), old);
        } else {
            self.local_bindings.remove("element");
        }

        // Extract event.press from element and expose at top level
        // This allows button.event.press to be the actual IOPad
        let event_field_id = self.event_loop.arena.intern_field("event");
        let press_field_id = self.event_loop.arena.intern_field("press");

        // When user writes: element: [event: [press: LINK]]
        // LINK compiles directly to an IOPad, so element.event.press IS the IOPad
        let ev_slot = self.get_field(element_slot, event_field_id);
        let iopad_slot = ev_slot.and_then(|ev| self.get_field(ev, press_field_id));

        #[cfg(target_arch = "wasm32")]
        zoon::println!("compile_element_button: element_slot={:?} ev_slot={:?} iopad_slot={:?}",
            element_slot, ev_slot, iopad_slot);

        // Create a new event Router with press pointing directly to the IOPad
        // IMPORTANT: Don't wrap in a Wire - we need the actual IOPad slot for event injection
        let event_slot = if let Some(iopad) = iopad_slot {
            #[cfg(target_arch = "wasm32")]
            zoon::println!("compile_element_button: FOUND IOPad {:?}, using directly (no wire wrapper)", iopad);
            self.compile_object(vec![(press_field_id, iopad)])
        } else {
            // Fallback: just expose element.event as-is
            #[cfg(target_arch = "wasm32")]
            zoon::println!("compile_element_button: FALLBACK - no IOPad found");
            self.get_field(element_slot, event_field_id)
                .unwrap_or_else(|| self.compile_constant(Payload::Unit))
        };

        // Create settings object
        let style_field_id = self.event_loop.arena.intern_field("style");
        let label_field_id = self.event_loop.arena.intern_field("label");

        let settings_slot = self.compile_object(vec![
            (style_field_id, style_slot),
            (label_field_id, label_slot),
        ]);

        // Create the tagged object: ElementButton[element, event, settings]
        let tag_id = self.event_loop.arena.intern_tag("ElementButton");
        let element_field_id = self.event_loop.arena.intern_field("element");
        let settings_field_id = self.event_loop.arena.intern_field("settings");

        let wrapper_slot = self.compile_object(vec![
            (element_field_id, element_slot),
            (event_field_id, event_slot),
            (settings_field_id, settings_slot),
        ]);

        // Create producer with TaggedObject payload
        let slot = self.event_loop.arena.alloc();
        if let Some(node) = self.event_loop.arena.get_mut(slot) {
            node.set_kind(NodeKind::Producer {
                value: Some(Payload::TaggedObject {
                    tag: tag_id,
                    fields: wrapper_slot,
                }),
            });
        }

        slot
    }

    /// Compile Element/label function call.
    fn compile_element_label(&mut self, arguments: &[Spanned<Argument<'code>>]) -> SlotId {
        // First compile `element` and add to local_bindings so other args can reference it
        let element_slot = self.get_argument_slot(arguments, "element");

        // Save old binding and add element to local_bindings
        let old_element_binding = self.local_bindings.insert("element".into(), element_slot);

        // Now compile other arguments that may reference element.hovered, etc.
        let style_slot = self.get_argument_slot(arguments, "style");
        let label_slot = self.get_argument_slot(arguments, "label");

        // Restore old binding
        if let Some(old) = old_element_binding {
            self.local_bindings.insert("element".into(), old);
        } else {
            self.local_bindings.remove("element");
        }

        let style_field_id = self.event_loop.arena.intern_field("style");
        let label_field_id = self.event_loop.arena.intern_field("label");

        let settings_slot = self.compile_object(vec![
            (style_field_id, style_slot),
            (label_field_id, label_slot),
        ]);

        let tag_id = self.event_loop.arena.intern_tag("ElementLabel");
        let element_field_id = self.event_loop.arena.intern_field("element");
        let settings_field_id = self.event_loop.arena.intern_field("settings");

        let wrapper_slot = self.compile_object(vec![
            (element_field_id, element_slot),
            (settings_field_id, settings_slot),
        ]);

        let slot = self.event_loop.arena.alloc();
        let payload = Payload::TaggedObject {
            tag: tag_id,
            fields: wrapper_slot,
        };
        #[cfg(target_arch = "wasm32")]
        zoon::println!("compile_element_label: slot={:?} payload={:?}", slot, payload);
        if let Some(node) = self.event_loop.arena.get_mut(slot) {
            node.set_kind(NodeKind::Producer {
                value: Some(payload),
            });
        }

        slot
    }

    /// Compile Element/container function call.
    fn compile_element_container(&mut self, arguments: &[Spanned<Argument<'code>>]) -> SlotId {
        // First compile `element` and add to local_bindings so other args can reference it
        let element_slot = self.get_argument_slot(arguments, "element");

        // Save old binding and add element to local_bindings
        let old_element_binding = self.local_bindings.insert("element".into(), element_slot);

        // Now compile other arguments that may reference element.hovered, etc.
        let style_slot = self.get_argument_slot(arguments, "style");
        let child_slot = self.get_argument_slot(arguments, "child");

        // Restore old binding
        if let Some(old) = old_element_binding {
            self.local_bindings.insert("element".into(), old);
        } else {
            self.local_bindings.remove("element");
        }

        let style_field_id = self.event_loop.arena.intern_field("style");
        let child_field_id = self.event_loop.arena.intern_field("child");

        let settings_slot = self.compile_object(vec![
            (style_field_id, style_slot),
            (child_field_id, child_slot),
        ]);

        let tag_id = self.event_loop.arena.intern_tag("ElementContainer");
        let element_field_id = self.event_loop.arena.intern_field("element");
        let settings_field_id = self.event_loop.arena.intern_field("settings");

        let wrapper_slot = self.compile_object(vec![
            (element_field_id, element_slot),
            (settings_field_id, settings_slot),
        ]);

        let slot = self.event_loop.arena.alloc();
        if let Some(node) = self.event_loop.arena.get_mut(slot) {
            node.set_kind(NodeKind::Producer {
                value: Some(Payload::TaggedObject {
                    tag: tag_id,
                    fields: wrapper_slot,
                }),
            });
        }

        slot
    }

    /// Compile Element/stack function call.
    /// Element/stack(element, style, layers) - a container that stacks layers on top of each other.
    fn compile_element_stack(&mut self, arguments: &[Spanned<Argument<'code>>]) -> SlotId {
        // First compile `element` and add to local_bindings so other args can reference it
        let element_slot = self.get_argument_slot(arguments, "element");

        // Save old binding and add element to local_bindings
        let old_element_binding = self.local_bindings.insert("element".into(), element_slot);

        // Now compile other arguments that may reference element.hovered, etc.
        let style_slot = self.get_argument_slot(arguments, "style");
        let layers_slot = self.get_argument_slot(arguments, "layers");

        // Restore old binding
        if let Some(old) = old_element_binding {
            self.local_bindings.insert("element".into(), old);
        } else {
            self.local_bindings.remove("element");
        }

        let style_field_id = self.event_loop.arena.intern_field("style");
        let layers_field_id = self.event_loop.arena.intern_field("layers");

        let settings_slot = self.compile_object(vec![
            (style_field_id, style_slot),
            (layers_field_id, layers_slot),
        ]);

        let tag_id = self.event_loop.arena.intern_tag("ElementStack");
        let element_field_id = self.event_loop.arena.intern_field("element");
        let settings_field_id = self.event_loop.arena.intern_field("settings");

        let wrapper_slot = self.compile_object(vec![
            (element_field_id, element_slot),
            (settings_field_id, settings_slot),
        ]);

        let slot = self.event_loop.arena.alloc();
        if let Some(node) = self.event_loop.arena.get_mut(slot) {
            node.set_kind(NodeKind::Producer {
                value: Some(Payload::TaggedObject {
                    tag: tag_id,
                    fields: wrapper_slot,
                }),
            });
        }

        slot
    }

    /// Compile Element/text_input function call.
    fn compile_element_text_input(&mut self, arguments: &[Spanned<Argument<'code>>]) -> SlotId {
        // First compile `element` and add to local_bindings so other args can reference it
        let element_slot = self.get_argument_slot(arguments, "element");

        // Save old binding and add element to local_bindings
        let old_element_binding = self.local_bindings.insert("element".into(), element_slot);

        // Now compile other arguments that may reference element.event, etc.
        let style_slot = self.get_argument_slot(arguments, "style");
        let text_slot = self.get_argument_slot(arguments, "text");
        let placeholder_slot = self.get_argument_slot(arguments, "placeholder");
        let focus_slot = self.get_argument_slot(arguments, "focus");

        // Restore old binding
        if let Some(old) = old_element_binding {
            self.local_bindings.insert("element".into(), old);
        } else {
            self.local_bindings.remove("element");
        }

        // Extract event.key_down and event.change IOPads from element
        // When user writes: element: [event: [key_down: LINK, change: LINK]]
        // LINK compiles directly to an IOPad, so element.event.key_down IS the IOPad
        let event_field_id = self.event_loop.arena.intern_field("event");
        let key_down_field_id = self.event_loop.arena.intern_field("key_down");
        let change_field_id = self.event_loop.arena.intern_field("change");

        // Get the IOPads directly from element.event.key_down and element.event.change
        let ev_slot = self.get_field(element_slot, event_field_id);
        let key_down_iopad = ev_slot.and_then(|ev| self.get_field(ev, key_down_field_id));
        let change_iopad = ev_slot.and_then(|ev| self.get_field(ev, change_field_id));

        // Create a new event Router with IOPads directly exposed
        // IMPORTANT: Don't wrap in Wire - we need the actual IOPad slots for event injection
        let mut event_fields = vec![];
        if let Some(iopad) = key_down_iopad {
            event_fields.push((key_down_field_id, iopad));
        }
        if let Some(iopad) = change_iopad {
            event_fields.push((change_field_id, iopad));
        }

        let event_slot = if !event_fields.is_empty() {
            self.compile_object(event_fields)
        } else {
            // Fallback: just expose element.event as-is
            ev_slot
                .map(|s| self.compile_wire(s))
                .unwrap_or_else(|| self.compile_constant(Payload::Unit))
        };

        let style_field_id = self.event_loop.arena.intern_field("style");
        let text_field_id = self.event_loop.arena.intern_field("text");
        let placeholder_field_id = self.event_loop.arena.intern_field("placeholder");
        let focus_field_id = self.event_loop.arena.intern_field("focus");

        let settings_slot = self.compile_object(vec![
            (style_field_id, style_slot),
            (text_field_id, text_slot),
            (placeholder_field_id, placeholder_slot),
            (focus_field_id, focus_slot),
        ]);

        let tag_id = self.event_loop.arena.intern_tag("ElementTextInput");
        let element_field_id = self.event_loop.arena.intern_field("element");
        let settings_field_id = self.event_loop.arena.intern_field("settings");

        let wrapper_slot = self.compile_object(vec![
            (element_field_id, element_slot),
            (event_field_id, event_slot),
            (text_field_id, text_slot),
            (settings_field_id, settings_slot),
        ]);

        let slot = self.event_loop.arena.alloc();
        if let Some(node) = self.event_loop.arena.get_mut(slot) {
            node.set_kind(NodeKind::Producer {
                value: Some(Payload::TaggedObject {
                    tag: tag_id,
                    fields: wrapper_slot,
                }),
            });
        }

        slot
    }

    /// Compile Element/checkbox function call.
    fn compile_element_checkbox(&mut self, arguments: &[Spanned<Argument<'code>>]) -> SlotId {
        #[cfg(target_arch = "wasm32")]
        zoon::println!("compile_element_checkbox CALLED");

        // First compile `element` and add to local_bindings so other args can reference it
        let element_slot = self.get_argument_slot(arguments, "element");

        // Save old binding and add element to local_bindings
        let old_element_binding = self.local_bindings.insert("element".into(), element_slot);

        // Now compile other arguments that may reference element.event, etc.
        let style_slot = self.get_argument_slot(arguments, "style");
        let checked_slot = self.get_argument_slot(arguments, "checked");
        let label_slot = self.get_argument_slot(arguments, "label");
        let icon_slot = self.get_argument_slot(arguments, "icon");

        // Restore old binding
        if let Some(old) = old_element_binding {
            self.local_bindings.insert("element".into(), old);
        } else {
            self.local_bindings.remove("element");
        }

        // Extract event.click from element and expose at top level
        // When user writes: element: [event: [click: LINK]]
        // LINK compiles directly to an IOPad, so element.event.click IS the IOPad
        let event_field_id = self.event_loop.arena.intern_field("event");
        let click_field_id = self.event_loop.arena.intern_field("click");

        // Get the IOPad directly from element.event.click
        let ev_slot = self.get_field(element_slot, event_field_id);
        let iopad_slot = ev_slot.and_then(|ev| self.get_field(ev, click_field_id));

        // Create a new event Router with click pointing directly to the IOPad
        // IMPORTANT: Don't wrap in Wire - we need the actual IOPad slot for event injection
        let event_slot = if let Some(iopad) = iopad_slot {
            self.compile_object(vec![(click_field_id, iopad)])
        } else {
            // Fallback: just expose element.event as-is
            self.get_field(element_slot, event_field_id)
                .unwrap_or_else(|| self.compile_constant(Payload::Unit))
        };

        let style_field_id = self.event_loop.arena.intern_field("style");
        let checked_field_id = self.event_loop.arena.intern_field("checked");
        let label_field_id = self.event_loop.arena.intern_field("label");
        let icon_field_id = self.event_loop.arena.intern_field("icon");

        let settings_slot = self.compile_object(vec![
            (style_field_id, style_slot),
            (checked_field_id, checked_slot),
            (label_field_id, label_slot),
            (icon_field_id, icon_slot),
        ]);

        let tag_id = self.event_loop.arena.intern_tag("ElementCheckbox");
        let element_field_id = self.event_loop.arena.intern_field("element");
        let settings_field_id = self.event_loop.arena.intern_field("settings");

        let wrapper_slot = self.compile_object(vec![
            (element_field_id, element_slot),
            (event_field_id, event_slot),
            (checked_field_id, checked_slot),
            (settings_field_id, settings_slot),
        ]);

        let slot = self.event_loop.arena.alloc();
        if let Some(node) = self.event_loop.arena.get_mut(slot) {
            node.set_kind(NodeKind::Producer {
                value: Some(Payload::TaggedObject {
                    tag: tag_id,
                    fields: wrapper_slot,
                }),
            });
        }

        slot
    }

    /// Compile Element/paragraph function call.
    /// Element/paragraph(element, style, contents) -> ElementParagraph[settings: [...]]
    fn compile_element_paragraph(&mut self, arguments: &[Spanned<Argument<'code>>]) -> SlotId {
        // First compile `element` and add to local_bindings so other args can reference it
        let element_slot = self.get_argument_slot(arguments, "element");

        // Save old binding and add element to local_bindings
        let old_element_binding = self.local_bindings.insert("element".into(), element_slot);

        // Now compile other arguments that may reference element.hovered, etc.
        let style_slot = self.get_argument_slot(arguments, "style");
        let contents_slot = self.get_argument_slot(arguments, "contents");

        // Restore old binding
        if let Some(old) = old_element_binding {
            self.local_bindings.insert("element".into(), old);
        } else {
            self.local_bindings.remove("element");
        }

        let style_field_id = self.event_loop.arena.intern_field("style");
        let contents_field_id = self.event_loop.arena.intern_field("contents");

        let settings_slot = self.compile_object(vec![
            (style_field_id, style_slot),
            (contents_field_id, contents_slot),
        ]);

        let tag_id = self.event_loop.arena.intern_tag("ElementParagraph");
        let element_field_id = self.event_loop.arena.intern_field("element");
        let settings_field_id = self.event_loop.arena.intern_field("settings");

        let wrapper_slot = self.compile_object(vec![
            (element_field_id, element_slot),
            (settings_field_id, settings_slot),
        ]);

        let slot = self.event_loop.arena.alloc();
        if let Some(node) = self.event_loop.arena.get_mut(slot) {
            node.set_kind(NodeKind::Producer {
                value: Some(Payload::TaggedObject {
                    tag: tag_id,
                    fields: wrapper_slot,
                }),
            });
        }

        slot
    }

    /// Compile Element/link function call.
    /// Element/link(element, style, label, to, new_tab) -> ElementLink[settings: [...]]
    fn compile_element_link(&mut self, arguments: &[Spanned<Argument<'code>>]) -> SlotId {
        // First compile `element` and add to local_bindings so other args can reference it
        let element_slot = self.get_argument_slot(arguments, "element");

        // Save old binding and add element to local_bindings
        let old_element_binding = self.local_bindings.insert("element".into(), element_slot);

        // Now compile other arguments that may reference element.hovered, etc.
        let style_slot = self.get_argument_slot(arguments, "style");
        let label_slot = self.get_argument_slot(arguments, "label");
        let to_slot = self.get_argument_slot(arguments, "to");
        let new_tab_slot = self.get_argument_slot(arguments, "new_tab");

        // Restore old binding
        if let Some(old) = old_element_binding {
            self.local_bindings.insert("element".into(), old);
        } else {
            self.local_bindings.remove("element");
        }

        // Extract hovered from element
        let hovered_field_id = self.event_loop.arena.intern_field("hovered");
        let hovered_slot = self.get_field(element_slot, hovered_field_id)
            .map(|s| self.compile_wire(s))
            .unwrap_or_else(|| self.compile_constant(Payload::Bool(false)));

        let style_field_id = self.event_loop.arena.intern_field("style");
        let label_field_id = self.event_loop.arena.intern_field("label");
        let to_field_id = self.event_loop.arena.intern_field("to");
        let new_tab_field_id = self.event_loop.arena.intern_field("new_tab");

        let settings_slot = self.compile_object(vec![
            (style_field_id, style_slot),
            (label_field_id, label_slot),
            (to_field_id, to_slot),
            (new_tab_field_id, new_tab_slot),
        ]);

        let tag_id = self.event_loop.arena.intern_tag("ElementLink");
        let element_field_id = self.event_loop.arena.intern_field("element");
        let settings_field_id = self.event_loop.arena.intern_field("settings");

        let wrapper_slot = self.compile_object(vec![
            (element_field_id, element_slot),
            (hovered_field_id, hovered_slot),
            (settings_field_id, settings_slot),
        ]);

        let slot = self.event_loop.arena.alloc();
        if let Some(node) = self.event_loop.arena.get_mut(slot) {
            node.set_kind(NodeKind::Producer {
                value: Some(Payload::TaggedObject {
                    tag: tag_id,
                    fields: wrapper_slot,
                }),
            });
        }

        slot
    }

    /// Helper: Get a slot for a named argument, or Unit if not found.
    fn get_argument_slot(&mut self, arguments: &[Spanned<Argument<'code>>], name: &str) -> SlotId {
        for arg in arguments {
            // Use deref coercion: &StrSlice -> &str via Deref trait
            let arg_name: &str = &arg.node.name;
            if arg_name == name {
                if let Some(ref value) = arg.node.value {
                    return self.compile_expression(value);
                }
            }
        }
        // Return Unit if argument not found
        self.compile_constant(Payload::Unit)
    }

    // ============================================================
    // List operations
    // ============================================================

    /// Compile List/count - returns the number of items in the list.
    /// Creates a runtime ListCount node that evaluates when the list changes.
    fn compile_list_count(&mut self, input_slot: SlotId) -> SlotId {
        // Create a runtime node that counts at evaluation time
        let slot = self.event_loop.arena.alloc();

        // Calculate initial count at compile time
        let input_kind = self.event_loop.arena.get(input_slot).and_then(|n| n.kind().cloned());
        #[cfg(target_arch = "wasm32")]
        zoon::println!("compile_list_count: input_slot={:?} input_kind={:?}", input_slot, input_kind);

        let initial_count = self.event_loop.count_list_items(input_slot, 0).unwrap_or(0.0);
        #[cfg(target_arch = "wasm32")]
        zoon::println!("  initial_count={}", initial_count);

        if let Some(node) = self.event_loop.arena.get_mut(slot) {
            node.set_kind(NodeKind::ListCount {
                source: Some(input_slot),
            });
            // Set initial value for render-time reads
            node.extension_mut().current_value = Some(Payload::Number(initial_count));
        }

        // Subscribe to input changes
        self.event_loop.routing.add_route(input_slot, slot, Port::Input(0));

        // If input is a FilteredView, also subscribe to condition slots for reactive updates
        if let Some(NodeKind::FilteredView { conditions, .. }) = input_kind.as_ref() {
            for (_item_slot, cond_slot) in conditions {
                self.event_loop.routing.add_route(*cond_slot, slot, Port::Input(0));
            }
        }

        slot
    }

    /// Compile List/is_empty - returns true if the list has no items.
    /// Creates a runtime ListIsEmpty node that evaluates when the list changes.
    fn compile_list_is_empty(&mut self, input_slot: SlotId) -> SlotId {
        // Create a runtime node that checks emptiness at evaluation time
        let slot = self.event_loop.arena.alloc();

        // Calculate initial emptiness at compile time
        let initial_count = self.event_loop.count_list_items(input_slot, 0).unwrap_or(0.0);
        let is_empty = initial_count == 0.0;

        if let Some(node) = self.event_loop.arena.get_mut(slot) {
            node.set_kind(NodeKind::ListIsEmpty {
                source: Some(input_slot),
            });
            // Set initial value for render-time reads
            node.extension_mut().current_value = Some(Payload::Bool(is_empty));
        }
        // Subscribe to input changes
        self.event_loop.routing.add_route(input_slot, slot, Port::Input(0));
        slot
    }

    /// Compile List/append - adds an item to the list reactively.
    /// Now uses template instantiation for proper LINK slot isolation per item.
    fn compile_list_append(
        &mut self,
        input_slot: SlotId,
        item_expr: Option<&Spanned<Expression<'code>>>,
    ) -> SlotId {
        // Find the target Bus
        let bus_slot = self.find_bus_slot(input_slot);

        if let Some(bus_slot) = bus_slot {
            // Handle the item expression with template instantiation
            let item_expr = match item_expr {
                Some(expr) => expr,
                None => {
                    // No item expression - append Unit
                    let unit_slot = self.compile_constant(Payload::Unit);
                    let appender_slot = self.event_loop.arena.alloc();
                    if let Some(node) = self.event_loop.arena.get_mut(appender_slot) {
                        node.set_kind(NodeKind::ListAppender {
                            bus_slot,
                            input: Some(unit_slot),
                            template_input: None,
                            template_output: None,
                        });
                    }
                    self.event_loop.routing.add_route(unit_slot, appender_slot, Port::Input(0));
                    return bus_slot;
                }
            };

            // Create template input wire (placeholder for item value, like List/map does)
            let template_input = self.event_loop.arena.alloc();
            if let Some(node) = self.event_loop.arena.get_mut(template_input) {
                node.set_kind(NodeKind::Wire { source: None });
            }

            #[cfg(target_arch = "wasm32")]
            zoon::println!("List/append TEMPLATE: template_input={:?}", template_input);

            // Check if item_expr is a Pipe expression
            // For `title_to_add |> new_todo()`:
            //   - trigger = title_to_add (the event stream that triggers append)
            //   - template = new_todo() applied to template_input
            let (trigger_slot, template_output) = match &item_expr.node {
                Expression::Pipe { from, to } => {
                    // Compile the trigger (left side of pipe)
                    let trigger = self.compile_expression(from);

                    // Compile the transform (right side of pipe) with template_input as input
                    let output = self.compile_pipe_to(template_input, to);

                    (trigger, output)
                }
                _ => {
                    // Not a pipe - compile the whole expression as the trigger
                    // The template will just forward the value (identity transform)
                    let trigger = self.compile_expression(item_expr);
                    (trigger, template_input)
                }
            };

            #[cfg(target_arch = "wasm32")]
            zoon::println!("List/append: trigger={:?} template_input={:?} template_output={:?}",
                trigger_slot, template_input, template_output);

            // Create a ListAppender node with template fields
            let appender_slot = self.event_loop.arena.alloc();
            if let Some(node) = self.event_loop.arena.get_mut(appender_slot) {
                node.set_kind(NodeKind::ListAppender {
                    bus_slot,
                    input: Some(trigger_slot),
                    template_input: Some(template_input),
                    template_output: Some(template_output),
                });
            }

            // Route from trigger to the appender
            self.event_loop.routing.add_route(trigger_slot, appender_slot, Port::Input(0));

            #[cfg(target_arch = "wasm32")]
            zoon::println!("compile_list_append: created ListAppender {:?} for bus {:?}, trigger {:?}",
                appender_slot, bus_slot, trigger_slot);

            // Return the bus slot (the list itself)
            bus_slot
        } else {
            // Create new bus with the item (for static case)
            let source_id = SourceId::default();
            let item_slot = if let Some(expr) = item_expr {
                self.compile_expression(expr)
            } else {
                self.compile_constant(Payload::Unit)
            };
            self.compile_list_items(source_id, vec![item_slot])
        }
    }

    /// Compile List/clear - clears all items when trigger fires.
    fn compile_list_clear(&mut self, input_slot: SlotId, trigger_slot: Option<SlotId>) -> SlotId {
        // Find the target Bus
        let bus_slot = self.find_bus_slot(input_slot);

        if let Some(bus_slot) = bus_slot {
            if let Some(trigger_slot) = trigger_slot {
                // Create a ListClearer node that will clear items when trigger emits
                let clearer_slot = self.event_loop.arena.alloc();
                if let Some(node) = self.event_loop.arena.get_mut(clearer_slot) {
                    node.set_kind(NodeKind::ListClearer {
                        bus_slot,
                        trigger: Some(trigger_slot),
                    });
                }

                // Route from trigger to the clearer
                self.event_loop.routing.add_route(trigger_slot, clearer_slot, Port::Input(0));

                #[cfg(target_arch = "wasm32")]
                zoon::println!("compile_list_clear: created ListClearer {:?} for bus {:?}, trigger {:?}",
                    clearer_slot, bus_slot, trigger_slot);
            }

            // Return the bus slot (the list itself)
            bus_slot
        } else {
            // No bus found, just return input
            input_slot
        }
    }

    /// Compile List/remove - removes items based on an event.
    fn compile_list_remove(
        &mut self,
        input_slot: SlotId,
        item_param: &str,
        on_expr: Option<&Spanned<Expression<'code>>>,
    ) -> SlotId {
        // For now, just compile the on_expr with proper item bindings (no-op for removal)
        // Full implementation would set up event handlers for removal
        if let Some(bus_slot) = self.find_bus_slot(input_slot) {
            // Get items from the bus
            let items: Vec<_> = if let Some(node) = self.event_loop.arena.get(bus_slot) {
                if let Some(NodeKind::Bus { items, .. }) = node.kind() {
                    items.clone()
                } else {
                    vec![]
                }
            } else {
                vec![]
            };

            // Compile on_expr for each item to avoid "unknown variable" errors
            for (_key, item_slot) in items {
                if let Some(expr) = on_expr {
                    let old_bindings = self.local_bindings.clone();
                    self.local_bindings.insert(item_param.to_string(), item_slot);
                    let _ = self.compile_expression(expr);
                    self.local_bindings = old_bindings;
                }
            }
        }

        self.compile_wire(input_slot)
    }

    /// Compile List/retain - filters the list based on a condition.
    /// Creates a FilteredView node that stores per-item visibility conditions.
    /// Now also creates a template for dynamic items added at runtime.
    fn compile_list_retain(
        &mut self,
        input_slot: SlotId,
        item_param: &str,
        if_expr: Option<&Spanned<Expression<'code>>>,
    ) -> SlotId {
        #[cfg(target_arch = "wasm32")]
        zoon::println!("=== compile_list_retain CALLED === input_slot={:?} item_param={}", input_slot, item_param);

        // Get the source Bus
        let bus_slot = self.find_bus_slot(input_slot);
        #[cfg(target_arch = "wasm32")]
        zoon::println!("  bus_slot={:?}", bus_slot);

        if let Some(bus_slot) = bus_slot {
            // Get items from the bus
            let items: Vec<_> = if let Some(node) = self.event_loop.arena.get(bus_slot) {
                if let Some(NodeKind::Bus { items, .. }) = node.kind() {
                    items.clone()
                } else {
                    vec![]
                }
            } else {
                vec![]
            };

            // Build per-item visibility conditions for this filtered view
            let mut conditions = std::collections::HashMap::new();

            for (_key, item_slot) in &items {
                // Bind item to parameter
                let old_bindings = self.local_bindings.clone();
                self.local_bindings.insert(item_param.to_string(), *item_slot);

                // Compile the visibility condition (reactive - will update when dependencies change)
                if let Some(expr) = if_expr {
                    let cond_slot = self.compile_expression(expr);
                    // Store in FilteredView's conditions, not globally
                    conditions.insert(*item_slot, cond_slot);
                    // Also store globally for rendering compatibility
                    self.event_loop.visibility_conditions.insert(*item_slot, cond_slot);
                }

                // Restore bindings
                self.local_bindings = old_bindings;
            }

            // Create template for dynamic items (if we have a filter expression)
            let (template_input, template_output) = if let Some(expr) = if_expr {
                // Create a Wire to receive the item slot at runtime
                let tmpl_input = self.event_loop.arena.alloc();
                if let Some(node) = self.event_loop.arena.get_mut(tmpl_input) {
                    node.set_kind(NodeKind::Wire { source: None });
                }

                // Bind template input to the item parameter
                let old_bindings = self.local_bindings.clone();
                self.local_bindings.insert(item_param.to_string(), tmpl_input);

                // Compile the condition expression with template input
                let tmpl_output = self.compile_expression(expr);

                // Restore bindings
                self.local_bindings = old_bindings;

                #[cfg(target_arch = "wasm32")]
                zoon::println!("  created template: input={:?} output={:?}", tmpl_input, tmpl_output);

                (Some(tmpl_input), Some(tmpl_output))
            } else {
                (None, None)
            };

            // Create a FilteredView node that wraps the Bus
            let filtered_slot = self.event_loop.arena.alloc();

            // Copy conditions before moving into node
            let cond_slots: Vec<SlotId> = conditions.values().copied().collect();

            if let Some(node) = self.event_loop.arena.get_mut(filtered_slot) {
                node.set_kind(NodeKind::FilteredView {
                    source_bus: bus_slot,
                    conditions,
                    template_input,
                    template_output,
                });
            }

            // Add route from source_bus to FilteredView so it gets notified of new items
            self.event_loop.routing.add_route(bus_slot, filtered_slot, Port::Input(0));

            // Trigger condition slots to run so Extractors can set up subscriptions
            for cond_slot in cond_slots {
                self.event_loop.mark_dirty(cond_slot, Port::Input(0));
            }

            #[cfg(target_arch = "wasm32")]
            zoon::println!("  created FilteredView {:?} -> bus {:?}", filtered_slot, bus_slot);

            filtered_slot
        } else {
            input_slot
        }
    }

    /// Compile List/map - transforms each item in the list.
    fn compile_list_map(
        &mut self,
        input_slot: SlotId,
        item_param: &str,
        new_expr: Option<&Spanned<Expression<'code>>>,
    ) -> SlotId {
        #[cfg(target_arch = "wasm32")]
        zoon::println!("=== compile_list_map CALLED === input_slot={:?} item_param={}", input_slot, item_param);

        // Get the source Bus
        let bus_slot = self.find_bus_slot(input_slot);
        #[cfg(target_arch = "wasm32")]
        zoon::println!("List/map: input_slot={:?} bus_slot={:?} item_param={} visibility_conds={}",
            input_slot, bus_slot, item_param, self.event_loop.visibility_conditions.len());

        if let Some(bus_slot) = bus_slot {
            // Get current items from the bus
            let items: Vec<_> = if let Some(node) = self.event_loop.arena.get(bus_slot) {
                if let Some(NodeKind::Bus { items, .. }) = node.kind() {
                    items.clone()
                } else {
                    vec![]
                }
            } else {
                vec![]
            };

            // Transform existing items using the old approach (compile each separately)
            // This ensures initial items work correctly
            let mut mapped_items = Vec::new();
            let mut initial_mapped = std::collections::HashMap::new();

            for (key, item_slot) in &items {
                // Bind item to parameter
                let old_bindings = self.local_bindings.clone();
                self.local_bindings.insert(item_param.to_string(), *item_slot);

                // Evaluate transform expression
                let new_slot = if let Some(expr) = new_expr {
                    self.compile_expression(expr)
                } else {
                    *item_slot
                };

                // Restore bindings
                self.local_bindings = old_bindings;

                #[cfg(target_arch = "wasm32")]
                zoon::println!("  List/map: key={} old_slot={:?} new_slot={:?}", key, item_slot, new_slot);

                // Propagate visibility conditions from source item to mapped item
                if let Some(cond_slot) = self.event_loop.visibility_conditions.get(item_slot).copied() {
                    self.event_loop.visibility_conditions.insert(new_slot, cond_slot);
                }

                mapped_items.push((*key, new_slot));
                initial_mapped.insert(*item_slot, new_slot);
            }

            // Create template for dynamic items (ListMapper will use this for new items)
            let template_input = self.event_loop.arena.alloc();
            if let Some(node) = self.event_loop.arena.get_mut(template_input) {
                node.set_kind(NodeKind::Wire { source: None });
            }

            #[cfg(target_arch = "wasm32")]
            zoon::println!("List/map TEMPLATE: template_input={:?} item_param='{}'", template_input, item_param);

            // Compile transform expression with item bound to template_input
            let old_bindings = self.local_bindings.clone();
            self.local_bindings.insert(item_param.to_string(), template_input);

            #[cfg(target_arch = "wasm32")]
            zoon::println!("  bindings after insert: {:?}", self.local_bindings.keys().collect::<Vec<_>>());

            let template_output = if let Some(expr) = new_expr {
                #[cfg(target_arch = "wasm32")]
                zoon::println!("  compiling new_expr...");
                self.compile_expression(expr)
            } else {
                template_input
            };

            #[cfg(target_arch = "wasm32")]
            zoon::println!("  template_output={:?}", template_output);

            self.local_bindings = old_bindings;

            #[cfg(target_arch = "wasm32")]
            zoon::println!("List/map: created template input={:?} output={:?}", template_input, template_output);

            // Create output Bus with mapped items
            let source_id = SourceId::default();
            let output_bus = self.compile_list(source_id);

            if let Some(node) = self.event_loop.arena.get_mut(output_bus) {
                if let Some(NodeKind::Bus { items, .. }) = node.kind_mut() {
                    *items = mapped_items;
                }
            }

            // Create ListMapper node to handle dynamic items
            let mapper_slot = self.event_loop.arena.alloc();
            if let Some(node) = self.event_loop.arena.get_mut(mapper_slot) {
                node.set_kind(NodeKind::ListMapper {
                    source_bus: bus_slot,
                    output_bus,
                    template_input,
                    template_output,
                    mapped_items: initial_mapped,
                });
            }

            // Subscribe ListMapper to source Bus
            self.event_loop.routing.add_route(bus_slot, mapper_slot, Port::Input(0));

            #[cfg(target_arch = "wasm32")]
            zoon::println!("List/map: created mapper={:?} output_bus={:?}", mapper_slot, output_bus);

            output_bus
        } else {
            input_slot
        }
    }

    /// Clone a transform subgraph from template_input to template_output,
    /// rewiring the clone's entry to use the given source_item.
    fn clone_transform_subgraph(
        &mut self,
        template_input: SlotId,
        template_output: SlotId,
        source_item: SlotId,
    ) -> SlotId {
        // Collect all slots in the subgraph from template_input to template_output
        let mut to_clone = std::collections::HashSet::new();
        let mut to_visit = vec![template_output];

        while let Some(slot) = to_visit.pop() {
            if to_clone.contains(&slot) {
                continue;
            }
            if slot == template_input {
                // Don't include template_input itself - we'll rewire to source_item
                continue;
            }
            to_clone.insert(slot);

            // Find dependencies of this slot
            if let Some(node) = self.event_loop.arena.get(slot) {
                match node.kind() {
                    Some(NodeKind::Wire { source: Some(src) }) => {
                        to_visit.push(*src);
                    }
                    Some(NodeKind::Transformer { input: Some(inp), body_slot }) => {
                        to_visit.push(*inp);
                        if let Some(body) = body_slot {
                            to_visit.push(*body);
                        }
                    }
                    Some(NodeKind::Router { fields }) => {
                        for (_, field_slot) in fields {
                            to_visit.push(*field_slot);
                        }
                    }
                    Some(NodeKind::Combiner { inputs, .. }) => {
                        for inp in inputs {
                            to_visit.push(*inp);
                        }
                    }
                    Some(NodeKind::Extractor { source: Some(src), .. }) => {
                        to_visit.push(*src);
                    }
                    Some(NodeKind::TextTemplate { dependencies, .. }) => {
                        for dep in dependencies {
                            to_visit.push(*dep);
                        }
                    }
                    Some(NodeKind::Arithmetic { left: Some(l), right: Some(r), .. }) => {
                        to_visit.push(*l);
                        to_visit.push(*r);
                    }
                    Some(NodeKind::Comparison { left: Some(l), right: Some(r), .. }) => {
                        to_visit.push(*l);
                        to_visit.push(*r);
                    }
                    Some(NodeKind::BoolNot { source: Some(src), .. }) => {
                        to_visit.push(*src);
                    }
                    // HOLD (Register) - traverse body and initial inputs
                    Some(NodeKind::Register { body_input, initial_input, .. }) => {
                        if let Some(body) = body_input {
                            to_visit.push(*body);
                        }
                        if let Some(init) = initial_input {
                            to_visit.push(*init);
                        }
                    }
                    // WHILE (SwitchedWire) - traverse input and arm bodies
                    Some(NodeKind::SwitchedWire { input, arms, .. }) => {
                        if let Some(inp) = input {
                            to_visit.push(*inp);
                        }
                        for (_, body_slot) in arms {
                            to_visit.push(*body_slot);
                        }
                    }
                    // WHEN (PatternMux) - traverse input and arm bodies
                    Some(NodeKind::PatternMux { input, arms, .. }) => {
                        if let Some(inp) = input {
                            to_visit.push(*inp);
                        }
                        for (_, body_slot) in arms {
                            to_visit.push(*body_slot);
                        }
                    }
                    // Bus - traverse all items
                    Some(NodeKind::Bus { items, .. }) => {
                        for (_, item_slot) in items {
                            to_visit.push(*item_slot);
                        }
                    }
                    // Effect - traverse input
                    Some(NodeKind::Effect { input, .. }) => {
                        if let Some(inp) = input {
                            to_visit.push(*inp);
                        }
                    }
                    // IOPad - traverse element slot
                    Some(NodeKind::IOPad { element_slot, .. }) => {
                        if let Some(el) = element_slot {
                            to_visit.push(*el);
                        }
                    }
                    _ => {}
                }
            }
        }

        // If nothing to clone (template_output == template_input), just return source_item
        if to_clone.is_empty() {
            return source_item;
        }

        // Create mapping from old slots to new slots
        let mut slot_map = std::collections::HashMap::new();
        slot_map.insert(template_input, source_item); // Rewire template_input to source_item

        for &old_slot in &to_clone {
            let new_slot = self.event_loop.arena.alloc();
            slot_map.insert(old_slot, new_slot);
        }

        // Clone each node with remapped references
        for &old_slot in &to_clone {
            let new_slot = slot_map[&old_slot];

            // Extract data from old node first (immutable borrow)
            let (new_kind, producer_value) = if let Some(old_node) = self.event_loop.arena.get(old_slot) {
                let kind = self.remap_node_kind(old_node.kind(), &slot_map);
                let value = if let Some(NodeKind::Producer { value }) = old_node.kind() {
                    value.clone()
                } else {
                    None
                };
                (kind, value)
            } else {
                (None, None)
            };

            // Now apply to new node (mutable borrow)
            if let Some(new_node) = self.event_loop.arena.get_mut(new_slot) {
                if let Some(kind) = new_kind {
                    new_node.set_kind(kind);
                }
                if let Some(value) = producer_value {
                    new_node.extension_mut().current_value = Some(value);
                }
            }
        }

        // Return the cloned output slot
        slot_map.get(&template_output).copied().unwrap_or(source_item)
    }

    /// Remap SlotIds inside a Payload.
    fn remap_payload(
        payload: &Payload,
        slot_map: &std::collections::HashMap<SlotId, SlotId>,
    ) -> Payload {
        let remap = |slot: &SlotId| -> SlotId {
            slot_map.get(slot).copied().unwrap_or(*slot)
        };

        match payload {
            Payload::ObjectHandle(slot) => Payload::ObjectHandle(remap(slot)),
            Payload::ListHandle(slot) => Payload::ListHandle(remap(slot)),
            Payload::TaggedObject { tag, fields } => Payload::TaggedObject {
                tag: *tag,
                fields: remap(fields),
            },
            other => other.clone(),
        }
    }

    /// Remap a NodeKind's slot references using the given mapping.
    fn remap_node_kind(
        &self,
        kind: Option<&NodeKind>,
        slot_map: &std::collections::HashMap<SlotId, SlotId>,
    ) -> Option<NodeKind> {
        let remap = |slot: &SlotId| -> SlotId {
            slot_map.get(slot).copied().unwrap_or(*slot)
        };
        let remap_opt = |slot: &Option<SlotId>| -> Option<SlotId> {
            slot.map(|s| remap(&s))
        };

        match kind {
            Some(NodeKind::Producer { value }) => {
                let new_value = value.as_ref().map(|v| Self::remap_payload(v, slot_map));
                Some(NodeKind::Producer { value: new_value })
            }
            Some(NodeKind::Wire { source }) => {
                Some(NodeKind::Wire { source: remap_opt(source) })
            }
            Some(NodeKind::Router { fields }) => {
                let new_fields = fields.iter()
                    .map(|(k, v)| (*k, remap(v)))
                    .collect();
                Some(NodeKind::Router { fields: new_fields })
            }
            Some(NodeKind::Combiner { inputs, last_values }) => {
                let new_inputs = inputs.iter().map(|s| remap(s)).collect();
                Some(NodeKind::Combiner {
                    inputs: new_inputs,
                    last_values: last_values.clone(),
                })
            }
            Some(NodeKind::Transformer { input, body_slot }) => {
                Some(NodeKind::Transformer {
                    input: remap_opt(input),
                    body_slot: remap_opt(body_slot),
                })
            }
            Some(NodeKind::Extractor { source, field, subscribed_field }) => {
                Some(NodeKind::Extractor {
                    source: remap_opt(source),
                    field: *field,
                    subscribed_field: remap_opt(subscribed_field),
                })
            }
            Some(NodeKind::TextTemplate { template, dependencies, cached }) => {
                let new_deps = dependencies.iter().map(|s| remap(s)).collect();
                Some(NodeKind::TextTemplate {
                    template: template.clone(),
                    dependencies: new_deps,
                    cached: cached.clone(),
                })
            }
            Some(NodeKind::Arithmetic { op, left, right, left_value, right_value }) => {
                Some(NodeKind::Arithmetic {
                    op: *op,
                    left: remap_opt(left),
                    right: remap_opt(right),
                    left_value: *left_value,
                    right_value: *right_value,
                })
            }
            Some(NodeKind::Comparison { op, left, right, left_value, right_value }) => {
                Some(NodeKind::Comparison {
                    op: *op,
                    left: remap_opt(left),
                    right: remap_opt(right),
                    left_value: left_value.clone(),
                    right_value: right_value.clone(),
                })
            }
            Some(NodeKind::BoolNot { source, cached }) => {
                Some(NodeKind::BoolNot {
                    source: remap_opt(source),
                    cached: *cached,
                })
            }
            // Register (HOLD)
            Some(NodeKind::Register { stored_value, body_input, initial_input, initial_received }) => {
                Some(NodeKind::Register {
                    stored_value: stored_value.clone(),
                    body_input: remap_opt(body_input),
                    initial_input: remap_opt(initial_input),
                    initial_received: *initial_received,
                })
            }
            // SwitchedWire (WHILE)
            Some(NodeKind::SwitchedWire { input, current_arm, arms }) => {
                let new_arms = arms.iter()
                    .map(|(pat, slot)| (pat.clone(), remap(slot)))
                    .collect();
                Some(NodeKind::SwitchedWire {
                    input: remap_opt(input),
                    current_arm: *current_arm,
                    arms: new_arms,
                })
            }
            // PatternMux (WHEN)
            Some(NodeKind::PatternMux { input, current_arm, arms }) => {
                let new_arms = arms.iter()
                    .map(|(pat, slot)| (pat.clone(), remap(slot)))
                    .collect();
                Some(NodeKind::PatternMux {
                    input: remap_opt(input),
                    current_arm: *current_arm,
                    arms: new_arms,
                })
            }
            // Bus
            Some(NodeKind::Bus { items, alloc_site, static_item_count }) => {
                let new_items = items.iter()
                    .map(|(key, slot)| (*key, remap(slot)))
                    .collect();
                Some(NodeKind::Bus {
                    items: new_items,
                    alloc_site: alloc_site.clone(),
                    static_item_count: *static_item_count,
                })
            }
            // Effect
            Some(NodeKind::Effect { effect_type, input }) => {
                Some(NodeKind::Effect {
                    effect_type: effect_type.clone(),
                    input: remap_opt(input),
                })
            }
            // IOPad
            Some(NodeKind::IOPad { element_slot, event_type, connected }) => {
                Some(NodeKind::IOPad {
                    element_slot: remap_opt(element_slot),
                    event_type: event_type.clone(),
                    connected: *connected,
                })
            }
            // ListAppender - must remap template slots for proper cloning
            Some(NodeKind::ListAppender { bus_slot, input, template_input, template_output }) => {
                Some(NodeKind::ListAppender {
                    bus_slot: remap(bus_slot),
                    input: remap_opt(input),
                    template_input: remap_opt(template_input),
                    template_output: remap_opt(template_output),
                })
            }
            _ => kind.cloned()
        }
    }

    /// Helper: Find the Bus slot from an input (follows wires, extractors, FilteredViews, etc).
    fn find_bus_slot(&self, input_slot: SlotId) -> Option<SlotId> {
        if let Some(node) = self.event_loop.arena.get(input_slot) {
            #[cfg(target_arch = "wasm32")]
            zoon::println!("find_bus_slot: input={:?} kind={:?}", input_slot, node.kind());
            match node.kind() {
                Some(NodeKind::Bus { .. }) => Some(input_slot),
                Some(NodeKind::FilteredView { source_bus, .. }) => {
                    // Follow through to the underlying Bus
                    self.find_bus_slot(*source_bus)
                }
                Some(NodeKind::Wire { source: Some(source) }) => self.find_bus_slot(*source),
                Some(NodeKind::Extractor { source: Some(source), field, .. }) => {
                    // Follow the extractor to the source object, then get the field
                    // The field value might be a list (Bus)
                    #[cfg(target_arch = "wasm32")]
                    zoon::println!("find_bus_slot: Extractor source={:?} field={}", source, field);
                    let src_node_opt = self.event_loop.arena.get(*source);
                    #[cfg(target_arch = "wasm32")]
                    zoon::println!("  src_node_opt.is_some()={}", src_node_opt.is_some());
                    if let Some(src_node) = src_node_opt {
                        #[cfg(target_arch = "wasm32")]
                        zoon::println!("  src_node kind: {:?}", src_node.kind());
                        if let Some(NodeKind::Router { fields }) = src_node.kind() {
                            #[cfg(target_arch = "wasm32")]
                            zoon::println!("  -> Router with {} fields", fields.len());
                            if let Some(&field_slot) = fields.get(field) {
                                return self.find_bus_slot(field_slot);
                            }
                        }
                        // Follow wires in source
                        if let Some(NodeKind::Wire { source: Some(wire_src) }) = src_node.kind() {
                            #[cfg(target_arch = "wasm32")]
                            zoon::println!("  -> Wire, following via extractor to {:?}", wire_src);
                            return self.find_bus_slot_via_extractor(*wire_src, *field);
                        }
                        #[cfg(target_arch = "wasm32")]
                        zoon::println!("  -> Not Router or Wire, returning None");
                    }
                    None
                }
                _ => None,
            }
        } else {
            None
        }
    }

    /// Helper: Find Bus via extractor path (follows wires then looks up field)
    fn find_bus_slot_via_extractor(&self, source_slot: SlotId, field: FieldId) -> Option<SlotId> {
        #[cfg(target_arch = "wasm32")]
        zoon::println!("find_bus_slot_via_extractor: source_slot={:?} field={}", source_slot, field);
        if let Some(node) = self.event_loop.arena.get(source_slot) {
            #[cfg(target_arch = "wasm32")]
            zoon::println!("  node kind: {:?}", node.kind());
            match node.kind() {
                Some(NodeKind::Router { fields }) => {
                    #[cfg(target_arch = "wasm32")]
                    zoon::println!("  -> Router, looking for field {} in {} fields", field, fields.len());
                    if let Some(&field_slot) = fields.get(&field) {
                        self.find_bus_slot(field_slot)
                    } else {
                        #[cfg(target_arch = "wasm32")]
                        zoon::println!("  -> Field not found!");
                        None
                    }
                }
                Some(NodeKind::Wire { source: Some(source) }) => {
                    #[cfg(target_arch = "wasm32")]
                    zoon::println!("  -> Wire, following to {:?}", source);
                    self.find_bus_slot_via_extractor(*source, field)
                }
                _ => {
                    // Check if current_value has an ObjectHandle we can follow
                    let cv = self.event_loop.get_current_value(source_slot);
                    #[cfg(target_arch = "wasm32")]
                    zoon::println!("  -> Other node type, current_value={:?}", cv);
                    if let Some(Payload::ObjectHandle(obj_slot)) = cv {
                        #[cfg(target_arch = "wasm32")]
                        zoon::println!("  -> Following ObjectHandle to {:?}", obj_slot);
                        return self.find_bus_slot_via_extractor(*obj_slot, field);
                    }
                    None
                }
            }
        } else {
            #[cfg(target_arch = "wasm32")]
            zoon::println!("  -> Node not found!");
            None
        }
    }

    /// Find a Router slot by following Wire chains.
    /// Used to find the object slot for HOLD initial values.
    fn find_router_slot(&self, slot: SlotId) -> Option<SlotId> {
        if let Some(node) = self.event_loop.arena.get(slot) {
            #[cfg(target_arch = "wasm32")]
            zoon::println!("  find_router_slot: slot={:?} kind={:?}", slot, node.kind());
            match node.kind() {
                Some(NodeKind::Router { .. }) => Some(slot),
                Some(NodeKind::Wire { source: Some(source) }) => self.find_router_slot(*source),
                _ => {
                    // Check current_value for ObjectHandle
                    if let Some(Payload::ObjectHandle(obj_slot)) = self.event_loop.get_current_value(slot) {
                        self.find_router_slot(*obj_slot)
                    } else {
                        None
                    }
                }
            }
        } else {
            None
        }
    }

    // ============================================================
    // Text operations
    // ============================================================

    /// Compile Text/trim - trims whitespace from both ends (reactive).
    fn compile_text_trim(&mut self, input_slot: SlotId) -> SlotId {
        // Check if input is a static constant - optimize to constant output
        if let Some(node) = self.event_loop.arena.get(input_slot) {
            if let Some(NodeKind::Producer { value: Some(Payload::Text(s)) }) = node.kind() {
                let trimmed = s.trim();
                return self.compile_constant(Payload::Text(Arc::from(trimmed)));
            }
        }

        // Create a reactive TextTrim node
        let slot = self.event_loop.arena.alloc();

        // Get initial value if available
        let initial_value = self.event_loop.get_current_value(input_slot)
            .and_then(|p| match p {
                Payload::Text(s) => Some(Payload::Text(Arc::from(s.trim()))),
                _ => None,
            });

        if let Some(node) = self.event_loop.arena.get_mut(slot) {
            node.set_kind(NodeKind::TextTrim {
                source: Some(input_slot),
            });
            if let Some(val) = initial_value {
                node.extension_mut().current_value = Some(val);
            }
        }

        // Subscribe to input changes
        self.event_loop.routing.add_route(input_slot, slot, Port::Input(0));

        slot
    }

    /// Compile Text/is_not_empty - returns true if text is not empty (reactive).
    fn compile_text_is_not_empty(&mut self, input_slot: SlotId) -> SlotId {
        // Check if input is a static constant - optimize to constant output
        if let Some(node) = self.event_loop.arena.get(input_slot) {
            if let Some(NodeKind::Producer { value: Some(Payload::Text(s)) }) = node.kind() {
                let is_not_empty = !s.is_empty();
                return self.compile_constant(Payload::Bool(is_not_empty));
            }
        }

        // Create a reactive TextIsNotEmpty node
        let slot = self.event_loop.arena.alloc();

        // Get initial value if available
        let initial_value = self.event_loop.get_current_value(input_slot)
            .and_then(|p| match p {
                Payload::Text(s) => Some(Payload::Bool(!s.is_empty())),
                _ => None,
            });

        if let Some(node) = self.event_loop.arena.get_mut(slot) {
            node.set_kind(NodeKind::TextIsNotEmpty {
                source: Some(input_slot),
            });
            if let Some(val) = initial_value {
                node.extension_mut().current_value = Some(val);
            }
        }

        // Subscribe to input changes
        self.event_loop.routing.add_route(input_slot, slot, Port::Input(0));

        slot
    }

    // ============================================================
    // Bool operations
    // ============================================================

    /// Compile Bool/not - logical NOT (reactive).
    fn compile_bool_not(&mut self, input_slot: SlotId) -> SlotId {
        // Check if input is a static constant - optimize to constant output
        if let Some(node) = self.event_loop.arena.get(input_slot) {
            if let Some(NodeKind::Producer { value: Some(Payload::Bool(b)) }) = node.kind() {
                return self.compile_constant(Payload::Bool(!*b));
            }
            // Handle Tag(True/False) as static constant
            if let Some(NodeKind::Producer { value: Some(Payload::Tag(tag_id)) }) = node.kind() {
                if let Some(tag_name) = self.event_loop.arena.get_tag_name(*tag_id) {
                    let result = tag_name.as_ref() != "True";
                    return self.compile_constant(Payload::Bool(result));
                }
            }
        }

        // Create a reactive BoolNot node for non-constant inputs
        let slot = self.event_loop.arena.alloc();

        // Get initial value
        let initial_value = self.event_loop.get_current_value(input_slot)
            .and_then(|p| match p {
                Payload::Bool(b) => Some(!*b),
                Payload::Tag(tag_id) => {
                    self.event_loop.arena.get_tag_name(*tag_id)
                        .map(|name| name.as_ref() != "True")
                }
                _ => None,
            });

        #[cfg(target_arch = "wasm32")]
        zoon::println!("compile_bool_not: input_slot={:?} -> slot={:?} initial={:?}", input_slot, slot, initial_value);

        if let Some(node) = self.event_loop.arena.get_mut(slot) {
            node.set_kind(NodeKind::BoolNot {
                source: Some(input_slot),
                cached: initial_value,
            });
            // Set initial current_value
            if let Some(b) = initial_value {
                node.extension_mut().current_value = Some(Payload::Bool(b));
            }
        }

        // Wire input to this BoolNot node for reactive updates
        self.event_loop.routing.add_route(input_slot, slot, Port::Input(0));

        slot
    }

    /// Compile Bool/or - logical OR.
    fn compile_bool_or(&mut self, input_slot: SlotId, that_slot: SlotId) -> SlotId {
        // Get both bool values and OR them
        let a = self.get_bool_value(input_slot);
        let b = self.get_bool_value(that_slot);
        self.compile_constant(Payload::Bool(a || b))
    }

    /// Helper: Get bool value from a slot.
    fn get_bool_value(&self, slot: SlotId) -> bool {
        if let Some(node) = self.event_loop.arena.get(slot) {
            match node.kind() {
                Some(NodeKind::Producer { value: Some(Payload::Bool(b)) }) => *b,
                Some(NodeKind::Producer { value: Some(Payload::Tag(tag_id)) }) => {
                    if let Some(tag_name) = self.event_loop.arena.get_tag_name(*tag_id) {
                        tag_name.as_ref() == "True"
                    } else {
                        false
                    }
                }
                Some(NodeKind::Wire { source: Some(source) }) => self.get_bool_value(*source),
                _ => false,
            }
        } else {
            false
        }
    }

    /// Get or create the global route slot (for Router/route()).
    pub fn get_or_create_route_slot(&mut self) -> SlotId {
        if let Some(slot) = self.event_loop.route_slot {
            // Return existing route slot
            slot
        } else {
            // Create a new Producer for the route with initial value "/"
            let slot = self.event_loop.arena.alloc();
            if let Some(node) = self.event_loop.arena.get_mut(slot) {
                node.set_kind(NodeKind::Producer {
                    value: Some(Payload::Text(Arc::from("/"))),
                });
                // Set initial value in extension
                if let Some(ext) = node.extension.as_mut() {
                    ext.current_value = Some(Payload::Text(Arc::from("/")));
                }
            }
            self.event_loop.route_slot = Some(slot);
            slot
        }
    }

    /// Compile Router/go_to - when input arrives, update the global route.
    pub fn compile_router_go_to(&mut self, input_slot: SlotId) -> SlotId {
        // Get or create the route slot (so it exists for Effect to update)
        let _route_slot = self.get_or_create_route_slot();

        // Create an Effect node for RouterGoTo
        // The effect will be processed by the bridge to:
        // 1. Update the route_slot Producer's value
        // 2. Call browser history.pushState
        let effect_slot = self.event_loop.arena.alloc();
        if let Some(node) = self.event_loop.arena.get_mut(effect_slot) {
            node.set_kind(NodeKind::Effect {
                effect_type: EffectType::RouterGoTo,
                input: Some(input_slot),
            });
        }

        // Route input to the effect
        self.event_loop.routing.add_route(input_slot, effect_slot, Port::Input(0));

        // Return the effect as the result (for the pipe chain)
        effect_slot
    }
}

#[cfg(test)]
impl CompileContext<'_, '_> {
    /// Create a probe node that stores received values (for testing).
    pub fn compile_probe(&mut self) -> SlotId {
        let slot = self.event_loop.arena.alloc();
        if let Some(node) = self.event_loop.arena.get_mut(slot) {
            node.set_kind(NodeKind::Probe { last: None });
        }
        slot
    }

    /// Get the last value received by a probe.
    pub fn get_probe_value(&self, slot: SlotId) -> Option<Payload> {
        self.event_loop.arena.get(slot)
            .and_then(|n| n.kind())
            .and_then(|k| match k {
                NodeKind::Probe { last } => last.clone(),
                _ => None,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine_v2::event_loop::EventLoop;

    #[test]
    fn compile_constant() {
        let mut el = EventLoop::new();
        let mut ctx = CompileContext::new(&mut el);

        let slot = ctx.compile_constant(Payload::Number(42.0));

        let node = ctx.event_loop.arena.get(slot).unwrap();
        match node.kind() {
            Some(NodeKind::Producer { value: Some(Payload::Number(n)) }) => {
                assert_eq!(*n, 42.0);
            }
            _ => panic!("Expected Producer with Number"),
        }
    }

    #[test]
    fn compile_object_field_access() {
        let mut el = EventLoop::new();
        let mut ctx = CompileContext::new(&mut el);

        // Create fields
        let field_a = ctx.compile_constant(Payload::Number(1.0));
        let field_b = ctx.compile_constant(Payload::Number(2.0));

        // Create object
        let obj = ctx.compile_object(vec![(1, field_a), (2, field_b)]);

        // Access field
        let got_a = ctx.get_field(obj, 1).unwrap();
        assert_eq!(got_a, field_a);
    }

    #[test]
    fn compile_latest() {
        let mut el = EventLoop::new();
        let mut ctx = CompileContext::new(&mut el);

        // Create two inputs
        let input_a = ctx.compile_constant(Payload::Number(1.0));
        let input_b = ctx.compile_constant(Payload::Number(2.0));

        // Create LATEST combiner
        let latest = ctx.compile_latest(vec![input_a, input_b]);

        // Check routes were set up
        let routes_a = ctx.event_loop.routing.get_subscribers(input_a);
        assert_eq!(routes_a.len(), 1);
        assert_eq!(routes_a[0].0, latest);

        let routes_b = ctx.event_loop.routing.get_subscribers(input_b);
        assert_eq!(routes_b.len(), 1);
        assert_eq!(routes_b[0].0, latest);
    }

    #[test]
    fn compile_hold() {
        let mut el = EventLoop::new();
        let mut ctx = CompileContext::new(&mut el);

        // Create HOLD with initial value
        let hold = ctx.compile_hold(Payload::Number(0.0), None, None);

        // Check node has Register kind with initial value
        let node = ctx.event_loop.arena.get(hold).unwrap();
        match node.kind() {
            Some(NodeKind::Register { stored_value: Some(Payload::Number(n)), .. }) => {
                assert_eq!(*n, 0.0);
            }
            _ => panic!("Expected Register with Number"),
        }
    }

    #[test]
    fn compile_then() {
        let mut el = EventLoop::new();
        let mut ctx = CompileContext::new(&mut el);

        // Create input and body
        let input = ctx.compile_constant(Payload::Number(1.0));
        let body = ctx.compile_constant(Payload::Number(2.0));

        // Create THEN transformer
        let then = ctx.compile_then(input, body);

        // Check route was set up
        let routes = ctx.event_loop.routing.get_subscribers(input);
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].0, then);
    }

    #[test]
    fn compile_timer() {
        let mut el = EventLoop::new();
        let mut ctx = CompileContext::new(&mut el);

        // Create a 1000ms timer
        let timer = ctx.compile_timer(1000.0);

        // Check node has Timer kind
        let node = ctx.event_loop.arena.get(timer).unwrap();
        match node.kind() {
            Some(NodeKind::Timer { interval_ms, active, .. }) => {
                assert_eq!(*interval_ms, 1000.0);
                assert!(*active);
            }
            _ => panic!("Expected Timer node"),
        }

        // Check timer is scheduled
        assert!(!ctx.event_loop.timer_queue.is_empty());
    }

    #[test]
    fn compile_accumulator() {
        let mut el = EventLoop::new();
        let mut ctx = CompileContext::new(&mut el);

        // Create an input and accumulator
        let input = ctx.compile_constant(Payload::Number(5.0));
        let acc = ctx.compile_accumulator(input);

        // Check node has Accumulator kind
        let node = ctx.event_loop.arena.get(acc).unwrap();
        match node.kind() {
            Some(NodeKind::Accumulator { sum, has_input }) => {
                assert_eq!(*sum, 0.0); // Initially zero
                assert!(!*has_input); // No input received yet
            }
            _ => panic!("Expected Accumulator node"),
        }

        // Check route was set up
        let routes = ctx.event_loop.routing.get_subscribers(input);
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].0, acc);
    }

    #[test]
    fn timer_emits_on_tick() {
        use crate::engine_v2::address::Port;

        let mut el = EventLoop::new();
        let mut ctx = CompileContext::new(&mut el);

        // Create a timer
        let timer = ctx.compile_timer(1000.0);

        // Create a probe to capture timer output
        let probe = ctx.compile_probe();
        ctx.event_loop.routing.add_route(timer, probe, Port::Input(0));

        // Mark timer dirty
        ctx.event_loop.mark_dirty(timer, Port::Output);

        // Run a tick
        ctx.event_loop.run_tick();

        // Check that timer emitted Unit
        match ctx.get_probe_value(probe) {
            Some(Payload::Unit) => {}
            other => panic!("Expected Unit, got {:?}", other),
        }
    }

    #[test]
    fn accumulator_sums_values() {
        use crate::engine_v2::address::Port;

        let mut el = EventLoop::new();
        let mut ctx = CompileContext::new(&mut el);

        // Create an accumulator
        let input = ctx.event_loop.arena.alloc();
        let acc = ctx.compile_accumulator(input);

        // Mark accumulator dirty to get initial value
        ctx.event_loop.mark_dirty(acc, Port::Output);
        ctx.event_loop.run_tick();

        // Check initial sum is 0
        match ctx.event_loop.get_current_value(acc) {
            Some(Payload::Number(n)) => assert_eq!(*n, 0.0),
            _ => panic!("Expected Number(0)"),
        }

        // Send a value
        ctx.event_loop.inbox.insert((acc, Port::Input(0)), Payload::Number(5.0));
        ctx.event_loop.mark_dirty(acc, Port::Input(0));
        ctx.event_loop.run_tick();

        // Check sum is 5
        match ctx.event_loop.get_current_value(acc) {
            Some(Payload::Number(n)) => assert_eq!(*n, 5.0),
            _ => panic!("Expected Number(5)"),
        }

        // Send another value
        ctx.event_loop.inbox.insert((acc, Port::Input(0)), Payload::Number(3.0));
        ctx.event_loop.mark_dirty(acc, Port::Input(0));
        ctx.event_loop.run_tick();

        // Check sum is 8
        match ctx.event_loop.get_current_value(acc) {
            Some(Payload::Number(n)) => assert_eq!(*n, 8.0),
            _ => panic!("Expected Number(8)"),
        }
    }
}
