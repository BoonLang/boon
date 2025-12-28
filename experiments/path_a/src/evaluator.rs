//! Evaluator for Path A engine.
//!
//! Compiles AST into arena nodes.

use crate::arena::{Arena, SlotId};
use crate::node::{Node, NodeKind};
use crate::template::TemplateRegistry;
use shared::ast::{Expr, ExprKind, Literal, Pattern, Program};
use shared::test_harness::Value;
use std::collections::HashMap;

/// Evaluation context during compilation
pub struct EvalContext<'a> {
    pub arena: &'a mut Arena,
    pub templates: &'a mut TemplateRegistry,
    /// Variable name to slot mapping
    pub bindings: HashMap<String, SlotId>,
    /// External dependencies found (for template capture analysis)
    pub external_refs: Vec<(String, SlotId)>,
    /// Current trigger slot (from enclosing THEN/WHEN/WHILE)
    pub trigger: Option<SlotId>,
}

impl<'a> EvalContext<'a> {
    pub fn new(arena: &'a mut Arena, templates: &'a mut TemplateRegistry) -> Self {
        Self {
            arena,
            templates,
            bindings: HashMap::new(),
            external_refs: Vec::new(),
            trigger: None,
        }
    }

    /// Look up a variable binding
    pub fn lookup(&self, name: &str) -> Option<SlotId> {
        self.bindings.get(name).copied()
    }

    /// Bind a variable to a slot
    pub fn bind(&mut self, name: impl Into<String>, slot: SlotId) {
        self.bindings.insert(name.into(), slot);
    }

    /// Create a child context for a nested scope
    pub fn child(&mut self) -> EvalContext<'_> {
        EvalContext {
            arena: self.arena,
            templates: self.templates,
            bindings: self.bindings.clone(),
            external_refs: Vec::new(),
            trigger: self.trigger,
        }
    }

    /// Create a child context with a specific trigger
    pub fn with_trigger(&mut self, trigger: SlotId) -> EvalContext<'_> {
        EvalContext {
            arena: self.arena,
            templates: self.templates,
            bindings: self.bindings.clone(),
            external_refs: Vec::new(),
            trigger: Some(trigger),
        }
    }
}

/// Compile a program into the arena
pub fn compile_program(program: &Program, arena: &mut Arena, templates: &mut TemplateRegistry) -> HashMap<String, SlotId> {
    let mut ctx = EvalContext::new(arena, templates);
    let mut top_level = HashMap::new();

    // First pass: allocate slots for ALL top-level bindings
    // This ensures forward references work (e.g., items can reference all_completed)
    let binding_slots: Vec<(String, SlotId)> = program.bindings
        .iter()
        .map(|(name, _)| {
            let slot = ctx.arena.alloc();
            ctx.bind(name.clone(), slot);
            top_level.insert(name.clone(), slot);
            (name.clone(), slot)
        })
        .collect();

    // Second pass: compile each expression into its pre-allocated slot
    for ((_, expr), (_, slot)) in program.bindings.iter().zip(binding_slots.iter()) {
        let node = compile_expr_to_node(expr, &mut ctx, *slot);
        ctx.arena.set_node(*slot, node);
    }

    top_level
}

/// Compile an expression into a slot
pub fn compile_expr(expr: &Expr, ctx: &mut EvalContext) -> SlotId {
    let slot = ctx.arena.alloc();
    let node = compile_expr_to_node(expr, ctx, slot);
    ctx.arena.set_node(slot, node);
    slot
}

/// Compile an expression to a node (slot already allocated)
fn compile_expr_to_node(expr: &Expr, ctx: &mut EvalContext, _slot: SlotId) -> Node {
    match &expr.kind {
        ExprKind::Literal(lit) => {
            let value = literal_to_value(lit);
            Node::new(NodeKind::Constant(value))
        }

        ExprKind::Variable(name) => {
            if let Some(source) = ctx.lookup(name) {
                // Wire to existing binding
                ctx.arena.add_subscriber(source, _slot);
                Node::new(NodeKind::Wire(source))
            } else {
                // External reference - record for capture
                ctx.external_refs.push((name.clone(), _slot));
                Node::new(NodeKind::Constant(Value::Skip))
            }
        }

        ExprKind::Path(base, field) => {
            let base_slot = compile_expr(base, ctx);
            ctx.arena.add_subscriber(base_slot, _slot);
            Node::new(NodeKind::Path {
                base: base_slot,
                field: field.clone(),
            })
        }

        ExprKind::Object(fields) => {
            // First pass: allocate slots and bind names (for forward references)
            let mut field_slots: Vec<(String, SlotId)> = Vec::new();
            for (name, _) in fields {
                let slot = ctx.arena.alloc();
                ctx.bind(name.clone(), slot);
                field_slots.push((name.clone(), slot));
            }

            // Second pass: compile expressions into allocated slots
            for ((_, expr), (_, slot)) in fields.iter().zip(field_slots.iter()) {
                let node = compile_expr_to_node(expr, ctx, *slot);
                ctx.arena.set_node(*slot, node);
                ctx.arena.add_subscriber(*slot, _slot);
            }

            Node::new(NodeKind::Object(field_slots))
        }

        ExprKind::List(items) => {
            let item_slots: Vec<SlotId> = items
                .iter()
                .map(|expr| {
                    let item_slot = compile_expr(expr, ctx);
                    ctx.arena.add_subscriber(item_slot, _slot);
                    item_slot
                })
                .collect();
            Node::new(NodeKind::List(item_slots))
        }

        ExprKind::Call(name, args) => {
            let arg_slots: Vec<SlotId> = args
                .iter()
                .map(|expr| {
                    let arg_slot = compile_expr(expr, ctx);
                    ctx.arena.add_subscriber(arg_slot, _slot);
                    arg_slot
                })
                .collect();
            Node::new(NodeKind::Call {
                name: name.clone(),
                args: arg_slots,
            })
        }

        ExprKind::Pipe(input, method, args) => {
            let input_slot = compile_expr(input, ctx);
            ctx.arena.add_subscriber(input_slot, _slot);
            let mut arg_slots = vec![input_slot];
            for arg in args {
                let arg_slot = compile_expr(arg, ctx);
                ctx.arena.add_subscriber(arg_slot, _slot);
                arg_slots.push(arg_slot);
            }
            Node::new(NodeKind::Call {
                name: method.clone(),
                args: arg_slots,
            })
        }

        ExprKind::Latest(exprs) => {
            let input_slots: Vec<SlotId> = exprs
                .iter()
                .map(|expr| {
                    let input_slot = compile_expr(expr, ctx);
                    ctx.arena.add_subscriber(input_slot, _slot);
                    input_slot
                })
                .collect();
            Node::new(NodeKind::Latest(input_slots))
        }

        ExprKind::Hold { initial, state_name, body } => {
            // Get initial value directly from the expression
            let initial_value = eval_constant(initial);

            // Create state slot as a Cell (mutable storage, not re-computed)
            let state_slot = ctx.arena.alloc();
            ctx.arena.set_node(state_slot, Node::new(NodeKind::Cell));
            ctx.arena.set_value(state_slot, initial_value.clone());

            // Bind state name for body compilation
            ctx.bind(state_name.clone(), state_slot);

            // Compile body
            let body_slot = compile_expr(body, ctx);
            // HOLD subscribes to body so it's notified when body value changes
            ctx.arena.add_subscriber(body_slot, _slot);

            Node::new(NodeKind::Hold {
                state: state_slot,
                body: body_slot,
                initial: initial_value,
            })
        }

        ExprKind::Then { input, body } => {
            let input_slot = compile_expr(input, ctx);
            ctx.arena.add_subscriber(input_slot, _slot);
            // Compile body with this THEN's input as the trigger
            let mut body_ctx = ctx.with_trigger(input_slot);
            let body_slot = compile_expr(body, &mut body_ctx);
            Node::new(NodeKind::Then {
                input: input_slot,
                body: body_slot,
            })
        }

        ExprKind::When { input, arms } => {
            let input_slot = compile_expr(input, ctx);
            ctx.arena.add_subscriber(input_slot, _slot);
            let compiled_arms: Vec<(String, SlotId)> = arms
                .iter()
                .map(|(pattern, body)| {
                    let pattern_name = pattern_to_name(pattern);
                    let body_slot = compile_expr(body, ctx);
                    (pattern_name, body_slot)
                })
                .collect();
            Node::new(NodeKind::When {
                input: input_slot,
                arms: compiled_arms,
            })
        }

        ExprKind::While { input, pattern, body } => {
            let input_slot = compile_expr(input, ctx);
            ctx.arena.add_subscriber(input_slot, _slot);
            let pattern_name = pattern_to_name(pattern);
            let body_slot = compile_expr(body, ctx);
            Node::new(NodeKind::While {
                input: input_slot,
                pattern: pattern_name,
                body: body_slot,
            })
        }

        ExprKind::Link(alias) => {
            let node = Node::new(NodeKind::Link { bound: None });
            if let Some(name) = alias {
                node.with_name(name.clone())
            } else {
                node
            }
        }

        ExprKind::Block { bindings, output } => {
            let mut child_ctx = ctx.child();
            let binding_slots: Vec<(String, SlotId)> = bindings
                .iter()
                .map(|(name, expr)| {
                    let binding_slot = compile_expr(expr, &mut child_ctx);
                    child_ctx.bind(name.clone(), binding_slot);
                    (name.clone(), binding_slot)
                })
                .collect();
            let output_slot = compile_expr(output, &mut child_ctx);
            Node::new(NodeKind::Block {
                bindings: binding_slots,
                output: output_slot,
            })
        }

        ExprKind::ListMap { list, item_name, template } => {
            let list_slot = compile_expr(list, ctx);
            ctx.arena.add_subscriber(list_slot, _slot);

            // Create template
            let template_id = ctx.templates.create();
            if let Some(tmpl) = ctx.templates.get_mut(template_id) {
                // Add item input
                let item_slot = ctx.arena.alloc();
                tmpl.add_input(item_name.clone(), item_slot);
                tmpl.ast = Some(template.as_ref().clone());
            }

            Node::new(NodeKind::ListMap {
                list: list_slot,
                template: template_id,
                instances: Vec::new(),
            })
        }

        ExprKind::ListAppend { list, item } => {
            let list_slot = compile_expr(list, ctx);
            ctx.arena.add_subscriber(list_slot, _slot);

            // Capture all current bindings for template instantiation
            let captures = ctx.bindings.clone();

            // Subscribe to trigger if present (from enclosing THEN)
            if let Some(trigger) = ctx.trigger {
                ctx.arena.add_subscriber(trigger, _slot);
            }

            Node::new(NodeKind::ListAppend {
                list: list_slot,
                trigger: ctx.trigger,
                item_template: Box::new(item.as_ref().clone()),
                captures,
                instances: Vec::new(),
                instantiated_count: 0,
            })
        }

        ExprKind::ListClear { list } => {
            let list_slot = compile_expr(list, ctx);
            ctx.arena.add_subscriber(list_slot, _slot);

            // Subscribe to trigger if present (from enclosing THEN)
            if let Some(trigger) = ctx.trigger {
                ctx.arena.add_subscriber(trigger, _slot);
            }

            Node::new(NodeKind::ListClear {
                list: list_slot,
                trigger: ctx.trigger,
            })
        }

        ExprKind::ListRemove { list, index } => {
            let list_slot = compile_expr(list, ctx);
            ctx.arena.add_subscriber(list_slot, _slot);

            let index_slot = compile_expr(index, ctx);
            ctx.arena.add_subscriber(index_slot, _slot);

            // Subscribe to trigger if present (from enclosing THEN)
            if let Some(trigger) = ctx.trigger {
                ctx.arena.add_subscriber(trigger, _slot);
            }

            Node::new(NodeKind::ListRemove {
                list: list_slot,
                index: index_slot,
                trigger: ctx.trigger,
            })
        }

        ExprKind::ListRetain { list, item_name, predicate } => {
            let list_slot = compile_expr(list, ctx);
            ctx.arena.add_subscriber(list_slot, _slot);

            // Capture all current bindings for predicate evaluation
            let captures = ctx.bindings.clone();

            // Subscribe to trigger if present (from enclosing THEN)
            if let Some(trigger) = ctx.trigger {
                ctx.arena.add_subscriber(trigger, _slot);
            }

            Node::new(NodeKind::ListRetain {
                list: list_slot,
                trigger: ctx.trigger,
                predicate_template: Box::new(predicate.as_ref().clone()),
                item_name: item_name.clone(),
                captures,
            })
        }
    }
}

/// Convert a literal to a runtime value
fn literal_to_value(lit: &Literal) -> Value {
    match lit {
        Literal::Int(v) => Value::Int(*v),
        Literal::Float(v) => Value::Float(*v),
        Literal::String(v) => Value::String(v.as_str().into()),
        Literal::Bool(v) => Value::Bool(*v),
        Literal::Unit => Value::Unit,
    }
}

/// Evaluate a constant expression during compilation
fn eval_constant(expr: &Expr) -> Value {
    match &expr.kind {
        ExprKind::Literal(lit) => literal_to_value(lit),
        ExprKind::List(items) => {
            Value::List(std::sync::Arc::new(items.iter().map(eval_constant).collect()))
        }
        ExprKind::Object(fields) => {
            let obj: std::collections::HashMap<String, Value> = fields
                .iter()
                .map(|(name, expr)| (name.clone(), eval_constant(expr)))
                .collect();
            Value::Object(std::sync::Arc::new(obj))
        }
        // For non-constant expressions, return Skip (will be computed at runtime)
        _ => Value::Skip,
    }
}

/// Convert a pattern to a name for matching
fn pattern_to_name(pattern: &Pattern) -> String {
    match pattern {
        Pattern::Bind(name) => name.clone(),
        Pattern::Literal(lit) => format!("{:?}", lit),
        Pattern::Object(_) => "object".to_string(),
        Pattern::Wildcard => "_".to_string(),
    }
}
