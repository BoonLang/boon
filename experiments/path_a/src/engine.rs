//! Engine for Path A: Dirty Propagation + Explicit Captures
//!
//! The engine runs a tick-based dirty propagation algorithm:
//! 1. Events mark slots as dirty
//! 2. Dirty slots are processed in topological order
//! 3. Each dirty slot recomputes its value
//! 4. If value changed, subscribers are marked dirty

use crate::arena::{Arena, SlotId};
use crate::evaluator::{compile_program, compile_expr, EvalContext};
use crate::ledger::{DeltaKind, Ledger};
use crate::node::{Node, NodeKind};
use crate::template::TemplateRegistry;
use crate::value::{is_skip, ops};
use shared::ast::Program;
use shared::test_harness::Value;
use std::collections::HashMap;

/// The Path A engine
#[allow(dead_code)]
pub struct Engine {
    /// Slot arena
    arena: Arena,
    /// Template registry
    templates: TemplateRegistry,
    /// Top-level bindings (name -> slot)
    top_level: HashMap<String, SlotId>,
    /// Current tick number
    tick: u64,
    /// Delta ledger
    ledger: Ledger,
    /// Pending events to inject
    pending_events: Vec<(String, Value)>,
    /// Active events for this tick (slot -> value)
    active_events: HashMap<SlotId, Value>,
    /// Slots that have already fired this tick (for THEN/WHEN/WHILE pulse semantics)
    fired_this_tick: std::collections::HashSet<SlotId>,
}

impl Engine {
    /// Create a new engine from a program
    pub fn new_from_program(program: &Program) -> Self {
        let mut arena = Arena::new();
        let mut templates = TemplateRegistry::new();
        let top_level = compile_program(program, &mut arena, &mut templates);

        let mut engine = Self {
            arena,
            templates,
            top_level,
            tick: 0,
            ledger: Ledger::new(),
            pending_events: Vec::new(),
            active_events: HashMap::new(),
            fired_this_tick: std::collections::HashSet::new(),
        };

        // Mark all slots dirty for initial evaluation
        for i in 0..engine.arena.len() {
            engine.arena.mark_dirty(SlotId(i as u32));
        }

        // Initial tick to compute values
        engine.tick();

        engine
    }

    /// Inject an event at a path
    pub fn inject(&mut self, path: &str, payload: Value) {
        self.pending_events.push((path.to_string(), payload));
    }

    /// Run one tick of the engine
    pub fn tick(&mut self) {
        self.tick += 1;

        // Clear events from previous tick
        self.active_events.clear();
        self.fired_this_tick.clear();

        // Process pending events
        for (path, payload) in std::mem::take(&mut self.pending_events) {
            self.inject_event(&path, payload);
        }

        // Phase 0: Check for ListAppend triggers and instantiate new items
        self.instantiate_triggered_list_appends();

        // Phase 1: Stabilize all non-pulse nodes
        // Multiple passes until no changes
        for pass in 0..20 {
            let mut changed = false;
            for i in 0..self.arena.len() {
                let slot = SlotId(i as u32);
                let old_value = self.arena.get_value(slot).clone();

                if let Some(node) = self.arena.get_node(slot).cloned() {
                    // LINK: use event value for entire tick
                    if let NodeKind::Link { .. } = &node.kind {
                        if let Some(event_value) = self.active_events.get(&slot) {
                            if old_value != *event_value {
                                self.arena.set_value(slot, event_value.clone());
                                changed = true;
                            }
                        } else if old_value != Value::Skip {
                            self.arena.set_value(slot, Value::Skip);
                            changed = true;
                        }
                        continue;
                    }

                    // Skip pulse nodes in stabilization phase
                    let is_pulse_node = matches!(
                        &node.kind,
                        NodeKind::Then { .. } | NodeKind::When { .. } | NodeKind::While { .. }
                    );
                    if is_pulse_node {
                        continue;
                    }

                    // Compute node value
                    let new_value = self.compute_node(&node, slot);

                    if old_value != new_value {
                        self.arena.set_value(slot, new_value);
                        changed = true;
                    }
                }
            }

            if !changed {
                break;
            }
        }

        // Phase 2: Fire pulse nodes (THEN/WHEN/WHILE) exactly once
        for i in 0..self.arena.len() {
            let slot = SlotId(i as u32);

            if let Some(node) = self.arena.get_node(slot).cloned() {
                let is_pulse_node = matches!(
                    &node.kind,
                    NodeKind::Then { .. } | NodeKind::When { .. } | NodeKind::While { .. }
                );

                if is_pulse_node {
                    let new_value = self.compute_node(&node, slot);
                    self.arena.set_value(slot, new_value.clone());

                    // If pulse produced value, mark as fired
                    if !is_skip(&new_value) {
                        self.fired_this_tick.insert(slot);
                    }
                }
            }
        }

        // Phase 3a: Propagate pulse results through LATEST to HOLD
        // (while pulse values are still available, before resetting to Skip)
        for _ in 0..10 {
            let mut changed = false;
            for i in 0..self.arena.len() {
                let slot = SlotId(i as u32);
                let old_value = self.arena.get_value(slot).clone();

                if let Some(node) = self.arena.get_node(slot).cloned() {
                    // Skip LINK (already handled)
                    if matches!(&node.kind, NodeKind::Link { .. }) {
                        continue;
                    }

                    // Skip pulse nodes for now (don't reset yet)
                    let is_pulse_node = matches!(
                        &node.kind,
                        NodeKind::Then { .. } | NodeKind::When { .. } | NodeKind::While { .. }
                    );
                    if is_pulse_node {
                        continue;
                    }

                    // Compute node value (LATEST will read from pulse nodes)
                    let new_value = self.compute_node(&node, slot);

                    // HOLD: update state
                    if let NodeKind::Hold { state, .. } = &node.kind {
                        if !is_skip(&new_value) {
                            let state_old = self.arena.get_value(*state).clone();
                            if state_old != new_value {
                                self.arena.set_value(*state, new_value.clone());
                                changed = true;
                            }
                        }
                    }

                    if old_value != new_value {
                        self.arena.set_value(slot, new_value);
                        changed = true;
                    }
                }
            }

            if !changed {
                break;
            }
        }

        // Phase 3b: Reset fired pulse nodes to Skip
        for i in 0..self.arena.len() {
            let slot = SlotId(i as u32);
            if self.fired_this_tick.contains(&slot) {
                self.arena.set_value(slot, Value::Skip);
            }
        }

        // Clear events after processing
        self.active_events.clear();
    }

    /// Instantiate new items for ListAppend nodes whose triggers fired
    fn instantiate_triggered_list_appends(&mut self) {
        // First, reset all LINK values to Skip, then set from active events
        for i in 0..self.arena.len() {
            let slot = SlotId(i as u32);
            if let Some(node) = self.arena.get_node(slot).cloned() {
                if let NodeKind::Link { .. } = &node.kind {
                    if let Some(event_value) = self.active_events.get(&slot) {
                        self.arena.set_value(slot, event_value.clone());
                    } else {
                        // Reset to Skip if no event this tick
                        self.arena.set_value(slot, Value::Skip);
                    }
                }
            }
        }

        // Evaluate Wire nodes to propagate LINK values
        for i in 0..self.arena.len() {
            let slot = SlotId(i as u32);
            if let Some(node) = self.arena.get_node(slot).cloned() {
                if let NodeKind::Wire(source) = &node.kind {
                    let source_value = self.arena.get_value(*source).clone();
                    self.arena.set_value(slot, source_value);
                }
            }
        }

        // Evaluate Path nodes so they can read LINK values
        for i in 0..self.arena.len() {
            let slot = SlotId(i as u32);
            if let Some(node) = self.arena.get_node(slot).cloned() {
                if let NodeKind::Path { base, field } = &node.kind {
                    let base_value = self.arena.get_value(*base);
                    let new_value = ops::get_field(base_value, field);
                    self.arena.set_value(slot, new_value);
                }
            }
        }

        // Collect slots that need instantiation (to avoid borrow issues)
        let mut to_instantiate: Vec<(SlotId, Box<shared::ast::Expr>, HashMap<String, SlotId>)> = Vec::new();

        for i in 0..self.arena.len() {
            let slot = SlotId(i as u32);
            if let Some(node) = self.arena.get_node(slot) {
                if let NodeKind::ListAppend { trigger, item_template, captures, .. } = &node.kind {
                    if let Some(trigger_slot) = trigger {
                        // Check if trigger has a non-Skip value
                        let trigger_value = self.arena.get_value(*trigger_slot);
                        if !is_skip(trigger_value) {
                            to_instantiate.push((slot, item_template.clone(), captures.clone()));
                        }
                    }
                }
            }
        }

        // Instantiate new items
        for (list_append_slot, item_template, captures) in to_instantiate {
            self.instantiate_list_item(list_append_slot, &item_template, &captures);
        }
    }

    /// Instantiate a new item for a ListAppend node
    fn instantiate_list_item(
        &mut self,
        list_append_slot: SlotId,
        item_template: &shared::ast::Expr,
        captures: &HashMap<String, SlotId>,
    ) {
        // Check if the item is a simple expression (non-Object)
        let is_simple = !matches!(&item_template.kind, shared::ast::ExprKind::Object(_));

        // For simple items, snapshot captured values first
        let snapshotted_bindings: HashMap<String, SlotId> = if is_simple {
            // Snapshot all captured values as Constants
            captures
                .iter()
                .map(|(name, slot)| {
                    let value = self.arena.get_value(*slot).clone();
                    let const_slot = self.arena.alloc();
                    self.arena.set_node(const_slot, Node::new(NodeKind::Constant(value.clone())));
                    self.arena.set_value(const_slot, value);
                    (name.clone(), const_slot)
                })
                .collect()
        } else {
            captures.clone()
        };

        // Create evaluation context with the appropriate bindings
        let mut ctx = EvalContext::new(&mut self.arena, &mut self.templates);
        ctx.bindings = snapshotted_bindings;

        // Compile the template (creates new slots for this instance)
        let instance_slot = compile_expr(item_template, &mut ctx);

        // Add the instance to the ListAppend node
        if let Some(node) = self.arena.get_node_mut(list_append_slot) {
            if let NodeKind::ListAppend { instances, instantiated_count, .. } = &mut node.kind {
                instances.push(instance_slot);
                *instantiated_count += 1;
            }
        }

        // Mark new slots dirty for evaluation
        self.arena.mark_dirty(instance_slot);
    }

    /// Inject an event at a path
    fn inject_event(&mut self, path: &str, payload: Value) {
        self.ledger.record(self.tick, DeltaKind::Event {
            path: path.to_string(),
            payload: payload.clone(),
        });

        // Parse path: "button.click" -> resolve "button", then access "click"
        let parts: Vec<&str> = path.split('.').collect();
        if parts.is_empty() {
            return;
        }

        // Find the target slot
        if let Some(&slot) = self.top_level.get(parts[0]) {
            // Build a nested object for the remaining path parts
            // e.g., "button.click" with payload Unit becomes { click: Unit }
            // Special case: "input.submit" with payload value creates { submit: Unit, value: payload }
            let event_value = if parts.len() > 1 {
                let last_part = *parts.last().unwrap();
                if last_part == "submit" && !matches!(payload, Value::Unit) {
                    // For form submissions, include both submit event and value
                    let mut inner: HashMap<String, Value> = HashMap::new();
                    inner.insert("submit".to_string(), Value::Unit);
                    inner.insert("value".to_string(), payload);
                    let mut current = Value::Object(inner);
                    // Wrap in outer path parts if any
                    for &part in parts[1..parts.len()-1].iter().rev() {
                        let mut outer: HashMap<String, Value> = HashMap::new();
                        outer.insert(part.to_string(), current);
                        current = Value::Object(outer);
                    }
                    current
                } else {
                    let mut current = payload;
                    // Build from innermost to outermost
                    for &part in parts[1..].iter().rev() {
                        let mut inner: HashMap<String, Value> = HashMap::new();
                        inner.insert(part.to_string(), current);
                        current = Value::Object(inner);
                    }
                    current
                }
            } else {
                payload
            };

            // Store event for this slot
            self.active_events.insert(slot, event_value);
            self.arena.mark_dirty(slot);

            // Mark subscribers dirty
            for &sub in self.arena.get_subscribers(slot).to_vec().iter() {
                self.arena.mark_dirty(sub);
            }
        }
    }

    /// Process a dirty slot
    fn process_slot(&mut self, slot: SlotId) {
        self.arena.clear_dirty(slot);

        let node = match self.arena.get_node(slot) {
            Some(n) => n.clone(),
            None => return,
        };

        // Special handling for HOLD: update state slot when body produces value
        if let NodeKind::Hold { state, body, initial } = &node.kind {
            let body_value = self.arena.get_value(*body);
            if !is_skip(body_value) {
                // Update the state slot
                self.arena.set_value(*state, body_value.clone());
            } else {
                // Initialize state if needed
                let state_val = self.arena.get_value(*state);
                if is_skip(state_val) {
                    self.arena.set_value(*state, initial.clone());
                }
            }
        }

        // Special handling for LINK: check for active events
        if let NodeKind::Link { .. } = &node.kind {
            if let Some(event_value) = self.active_events.get(&slot) {
                let new_value = event_value.clone();
                self.arena.set_value(slot, new_value.clone());
                // Mark subscribers dirty
                for &sub in self.arena.get_subscribers(slot).to_vec().iter() {
                    self.arena.mark_dirty(sub);
                }
                return;
            }
        }

        let old_value = self.arena.get_value(slot).clone();
        let new_value = self.compute_node(&node, slot);

        if new_value != old_value {
            self.ledger.record(self.tick, DeltaKind::Set {
                slot,
                old_value: old_value.clone(),
                new_value: new_value.clone(),
            });

            self.arena.set_value(slot, new_value);

            // Mark subscribers dirty
            for &sub in self.arena.get_subscribers(slot).to_vec().iter() {
                self.arena.mark_dirty(sub);
            }
        }
    }

    /// Compute the value for a node
    fn compute_node(&self, node: &crate::node::Node, slot: SlotId) -> Value {
        match &node.kind {
            NodeKind::Constant(v) => v.clone(),

            // Cell nodes just return their stored value - they're mutable storage
            NodeKind::Cell => self.arena.get_value(slot).clone(),

            NodeKind::Wire(source) => self.arena.get_value(*source).clone(),

            NodeKind::Latest(inputs) => {
                // Take the most recent non-SKIP value
                let mut result = Value::Skip;
                for &input in inputs {
                    let v = self.arena.get_value(input);
                    if !is_skip(v) {
                        result = v.clone();
                    }
                }
                result
            }

            NodeKind::Hold { state, body, initial } => {
                // Get current state value (initialize if needed)
                let current_state = {
                    let state_val = self.arena.get_value(*state);
                    if is_skip(state_val) {
                        // Initialize state
                        initial.clone()
                    } else {
                        state_val.clone()
                    }
                };

                let body_value = self.arena.get_value(*body);
                if is_skip(body_value) {
                    // If state is a list and body is a THEN, check if THEN's body (ListAppend) has updated values
                    // This is needed for toggle_all to propagate changes to item instances
                    if let Value::List(_) = &current_state {
                        if let Some(body_node) = self.arena.get_node(*body) {
                            if let NodeKind::Then { body: then_body, .. } = &body_node.kind {
                                // Read from THEN's body (should be ListAppend) - it has the current instance values
                                let list_value = self.arena.get_value(*then_body);
                                if let Value::List(_) = list_value {
                                    return list_value.clone();
                                }
                            }
                        }
                    }
                    current_state
                } else {
                    // Body produced a value - this IS the new state
                    body_value.clone()
                }
            }

            NodeKind::Then { input, body } => {
                let input_value = self.arena.get_value(*input);
                if is_skip(input_value) {
                    Value::Skip
                } else {
                    self.arena.get_value(*body).clone()
                }
            }

            NodeKind::When { input, arms } => {
                let input_value = self.arena.get_value(*input);
                if is_skip(input_value) {
                    return Value::Skip;
                }

                for (pattern, body_slot) in arms {
                    if pattern_matches(pattern, input_value) {
                        return self.arena.get_value(*body_slot).clone();
                    }
                }
                Value::Skip
            }

            NodeKind::While { input, pattern, body } => {
                let input_value = self.arena.get_value(*input);
                if is_skip(input_value) {
                    return Value::Skip;
                }

                if pattern_matches(pattern, input_value) {
                    self.arena.get_value(*body).clone()
                } else {
                    Value::Skip
                }
            }

            NodeKind::Link { bound } => {
                // Check for active event first
                // We need the slot ID, but we only have the node...
                // For now, check if any event matches based on bound target
                match bound {
                    Some(target) => self.arena.get_value(*target).clone(),
                    None => Value::Skip,
                }
            }

            NodeKind::Object(fields) => {
                let obj: HashMap<String, Value> = fields
                    .iter()
                    .map(|(name, field_slot)| {
                        let val = self.arena.get_value(*field_slot).clone();
                        (name.clone(), val)
                    })
                    .collect();
                Value::Object(obj)
            }

            NodeKind::List(items) => {
                let list: Vec<Value> = items
                    .iter()
                    .map(|slot| self.arena.get_value(*slot).clone())
                    .collect();
                Value::List(list)
            }

            NodeKind::Path { base, field } => {
                let base_value = self.arena.get_value(*base);
                ops::get_field(base_value, field)
            }

            NodeKind::Call { name, args } => {
                let arg_values: Vec<Value> = args
                    .iter()
                    .map(|slot| self.arena.get_value(*slot).clone())
                    .collect();
                self.call_builtin(name, &arg_values)
            }

            NodeKind::ListMap { list: _, template: _, instances } => {
                // Return list of instance values
                let list: Vec<Value> = instances
                    .iter()
                    .map(|slot| self.arena.get_value(*slot).clone())
                    .collect();
                Value::List(list)
            }

            NodeKind::ListAppend { instances, .. } => {
                // Return list of values from all instantiated items
                let list: Vec<Value> = instances
                    .iter()
                    .map(|slot| self.arena.get_value(*slot).clone())
                    .collect();
                Value::List(list)
            }

            NodeKind::Block { bindings: _, output } => {
                self.arena.get_value(*output).clone()
            }
        }
    }

    /// Call a built-in function
    fn call_builtin(&self, name: &str, args: &[Value]) -> Value {
        match name {
            "add" => {
                if args.len() >= 2 {
                    ops::add(&args[0], &args[1])
                } else {
                    Value::Skip
                }
            }
            "Bool/not" => {
                if args.len() >= 1 {
                    ops::bool_not(&args[0])
                } else {
                    Value::Skip
                }
            }
            "List/len" => {
                if args.len() >= 1 {
                    ops::list_len(&args[0])
                } else {
                    Value::Skip
                }
            }
            "List/append" => {
                if args.len() >= 2 {
                    ops::list_append(&args[0], args[1].clone())
                } else {
                    Value::Skip
                }
            }
            "List/every" => {
                if args.len() >= 1 {
                    // Simplified: just check if all items are truthy
                    ops::list_every(&args[0], |v| {
                        matches!(v, Value::Bool(true))
                    })
                } else {
                    Value::Skip
                }
            }
            "Math/sum" => {
                if args.len() >= 1 {
                    match &args[0] {
                        Value::List(items) => {
                            let sum: i64 = items.iter()
                                .filter_map(|v| v.as_int())
                                .sum();
                            Value::Int(sum)
                        }
                        _ => Value::Skip
                    }
                } else {
                    Value::Skip
                }
            }
            _ => Value::Skip,
        }
    }

    /// Read a value at a path
    pub fn read(&self, path: &str) -> Value {
        let parts: Vec<&str> = path.split('.').collect();
        if parts.is_empty() {
            return Value::Skip;
        }

        // Handle array indexing: "todos[0].completed"
        let first_part = parts[0];
        let (name, index) = if let Some(bracket_pos) = first_part.find('[') {
            let name = &first_part[..bracket_pos];
            let index_str = &first_part[bracket_pos + 1..first_part.len() - 1];
            let index: usize = index_str.parse().unwrap_or(0);
            (name, Some(index))
        } else {
            (first_part, None)
        };

        let slot = match self.top_level.get(name) {
            Some(&s) => s,
            None => return Value::Skip,
        };

        let mut value = self.arena.get_value(slot).clone();

        // Apply array index if present
        if let Some(idx) = index {
            value = match value {
                Value::List(items) => items.get(idx).cloned().unwrap_or(Value::Skip),
                _ => Value::Skip,
            };
        }

        // Navigate remaining path
        for part in &parts[1..] {
            // Handle array indexing in path parts
            let (field, idx) = if let Some(bracket_pos) = part.find('[') {
                let field = &part[..bracket_pos];
                let index_str = &part[bracket_pos + 1..part.len() - 1];
                let index: usize = index_str.parse().unwrap_or(0);
                (field, Some(index))
            } else {
                (*part, None)
            };

            value = ops::get_field(&value, field);

            if let Some(i) = idx {
                value = match value {
                    Value::List(items) => items.get(i).cloned().unwrap_or(Value::Skip),
                    _ => Value::Skip,
                };
            }
        }

        value
    }

    /// Enable ledger recording
    pub fn enable_ledger(&mut self) {
        self.ledger.enable();
    }

    /// Get ledger entries
    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }
}

/// Check if a pattern matches a value
fn pattern_matches(pattern: &str, value: &Value) -> bool {
    match pattern {
        "_" => true,
        "True" => matches!(value, Value::Bool(true)),
        "False" => matches!(value, Value::Bool(false)),
        _ => {
            // Bind pattern - always matches
            if pattern.chars().next().map(|c| c.is_lowercase()).unwrap_or(false) {
                true
            } else {
                // Check literal match
                match value {
                    Value::String(s) => pattern == format!("\"{}\"", s),
                    Value::Int(i) => pattern == i.to_string(),
                    _ => false,
                }
            }
        }
    }
}

// Implement the Engine trait from shared
impl shared::test_harness::Engine for Engine {
    fn new(program: &Program) -> Self {
        Self::new_from_program(program)
    }

    fn inject(&mut self, path: &str, payload: Value) {
        Engine::inject(self, path, payload)
    }

    fn tick(&mut self) {
        Engine::tick(self)
    }

    fn read(&self, path: &str) -> Value {
        Engine::read(self, path)
    }
}
