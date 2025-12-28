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
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

/// Wrapper for SlotId with reverse ordering by topo_index (min-heap behavior)
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct TopoSlot {
    slot: SlotId,
    topo_index: u32,
}

impl Ord for TopoSlot {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse ordering: lower topo_index = higher priority
        other.topo_index.cmp(&self.topo_index)
    }
}

impl PartialOrd for TopoSlot {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

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
    /// Topological order of slots (dependencies before dependents)
    /// Computed after compilation, allows single-pass evaluation
    topo_order: Vec<SlotId>,
    // === A4 Optimization: Pre-classified node type indices ===
    /// LINK slots - for Phase 0 (prepare_link_values)
    link_slots: Vec<SlotId>,
    /// Pulse slots (THEN/WHEN/WHILE) - for Phase 2
    pulse_slots: Vec<SlotId>,
    /// HOLD slots - for Phase 3.5 (refresh_hold_list_states)
    hold_slots: Vec<SlotId>,
    /// ListAppend slots - for Phase 1.5 (instantiate_triggered_list_appends)
    list_append_slots: Vec<SlotId>,
}

impl Engine {
    /// Create a new engine from a program
    pub fn new_from_program(program: &Program) -> Self {
        let mut arena = Arena::new();
        let mut templates = TemplateRegistry::new();
        let top_level = compile_program(program, &mut arena, &mut templates);

        // Compute topological order after compilation
        let topo_order = arena.compute_topo_order();

        // A4: Pre-classify nodes by type for O(k) iteration instead of O(n)
        let mut link_slots = Vec::new();
        let mut pulse_slots = Vec::new();
        let mut hold_slots = Vec::new();
        let mut list_append_slots = Vec::new();

        for i in 0..arena.len() {
            let slot = SlotId(i as u32);
            if let Some(node) = arena.get_node(slot) {
                match &node.kind {
                    NodeKind::Link { .. } => link_slots.push(slot),
                    NodeKind::Then { .. } | NodeKind::When { .. } | NodeKind::While { .. } => {
                        pulse_slots.push(slot);
                    }
                    NodeKind::Hold { .. } => hold_slots.push(slot),
                    NodeKind::ListAppend { .. } => list_append_slots.push(slot),
                    _ => {}
                }
            }
        }

        // Sort pulse_slots by topo_index for Phase 2 (must process in dependency order)
        pulse_slots.sort_by_key(|slot| arena.get_topo_index(*slot));

        let mut engine = Self {
            arena,
            templates,
            top_level,
            tick: 0,
            ledger: Ledger::new(),
            pending_events: Vec::new(),
            active_events: HashMap::new(),
            fired_this_tick: std::collections::HashSet::new(),
            topo_order,
            link_slots,
            pulse_slots,
            hold_slots,
            list_append_slots,
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

    /// Run one tick of the engine using work-queue based dirty propagation
    pub fn tick(&mut self) {
        self.tick += 1;

        // Clear events from previous tick
        self.active_events.clear();
        self.fired_this_tick.clear();

        // Process pending events
        for (path, payload) in std::mem::take(&mut self.pending_events) {
            self.inject_event(&path, payload);
        }

        // Phase 0: Prepare LINK values (so Path nodes can read them)
        self.prepare_link_values();

        // Phase 1: Process dirty slots using work queue (non-pulse only)
        // Uses topological ordering via sorted queue
        self.process_dirty_queue_non_pulse();

        // Phase 1.5: Check for ListAppend triggers and instantiate new items
        // Must happen AFTER Phase 1 (Path values computed) but BEFORE Phase 2 (pulse nodes fire)
        // Triggers are usually Path nodes (e.g., button.click) whose values are computed in Phase 1
        self.instantiate_triggered_list_appends();

        // Phase 2: Fire pulse nodes (THEN/WHEN/WHILE) exactly once (in topo order)
        // A4: Uses pre-classified and sorted pulse_slots for O(k) instead of O(n)
        for &slot in &self.pulse_slots {
            if let Some(node) = self.arena.get_node(slot).cloned() {
                let new_value = self.compute_node(&node, slot);
                let old_value = self.arena.get_value(slot).clone();

                if new_value != old_value {
                    self.arena.set_value(slot, new_value.clone());
                    // Mark subscribers dirty for phase 3
                    self.mark_subscribers_dirty(slot);
                }

                // If pulse produced value, mark as fired
                if !is_skip(&new_value) {
                    self.fired_this_tick.insert(slot);
                }
            }
        }

        // Phase 3: Propagate pulse results through LATEST to HOLD using work queue
        self.process_dirty_queue_post_pulse();

        // Phase 3.5: Refresh HOLD states that contain lists with updated objects
        // This handles the case where nested Objects changed but THEN didn't fire
        self.refresh_hold_list_states();

        // Phase 4: Reset fired pulse nodes to Skip
        for slot in self.fired_this_tick.iter().copied().collect::<Vec<_>>() {
            self.arena.set_value(slot, Value::Skip);
        }

        // Clear events after processing
        self.active_events.clear();
    }

    /// Process dirty queue for non-pulse nodes (Phase 1)
    fn process_dirty_queue_non_pulse(&mut self) {
        // Build priority queue (min-heap by topo_index)
        let mut dirty_heap: BinaryHeap<TopoSlot> = BinaryHeap::new();

        // Collect all dirty slots into heap
        for slot in self.arena.dirty_slots() {
            dirty_heap.push(TopoSlot {
                slot,
                topo_index: self.arena.get_topo_index(slot),
            });
        }

        // Process queue (always gets lowest topo_index first)
        while let Some(TopoSlot { slot, .. }) = dirty_heap.pop() {
            // Skip if already processed (cleared dirty flag)
            if !self.arena.is_dirty(slot) {
                continue;
            }
            self.arena.clear_dirty(slot);

            if let Some(node) = self.arena.get_node(slot).cloned() {
                // Skip pulse nodes - they're handled in phase 2
                let is_pulse_node = matches!(
                    &node.kind,
                    NodeKind::Then { .. } | NodeKind::When { .. } | NodeKind::While { .. }
                );
                if is_pulse_node {
                    continue;
                }

                let old_value = self.arena.get_value(slot).clone();

                // LINK: use event value for entire tick
                let new_value = if let NodeKind::Link { .. } = &node.kind {
                    if let Some(event_value) = self.active_events.get(&slot) {
                        event_value.clone()
                    } else {
                        Value::Skip
                    }
                } else {
                    self.compute_node(&node, slot)
                };

                if old_value != new_value {
                    self.arena.set_value(slot, new_value);
                    // Mark subscribers dirty and add to heap
                    self.add_subscribers_to_heap(&mut dirty_heap, slot);
                }
            }
        }
    }

    /// Process dirty queue for post-pulse propagation (Phase 3)
    fn process_dirty_queue_post_pulse(&mut self) {
        // Build priority queue (min-heap by topo_index)
        let mut dirty_heap: BinaryHeap<TopoSlot> = BinaryHeap::new();

        for slot in self.arena.dirty_slots() {
            dirty_heap.push(TopoSlot {
                slot,
                topo_index: self.arena.get_topo_index(slot),
            });
        }

        // Process queue (always gets lowest topo_index first)
        while let Some(TopoSlot { slot, .. }) = dirty_heap.pop() {
            if !self.arena.is_dirty(slot) {
                continue;
            }
            self.arena.clear_dirty(slot);

            if let Some(node) = self.arena.get_node(slot).cloned() {
                // Skip LINK (already handled)
                if matches!(&node.kind, NodeKind::Link { .. }) {
                    continue;
                }

                let old_value = self.arena.get_value(slot).clone();

                // For pulse nodes in Phase 3: if they already fired this tick, they should
                // pass through updated body values. If they didn't fire, they stay Skip.
                let is_pulse_node = matches!(
                    &node.kind,
                    NodeKind::Then { .. } | NodeKind::When { .. } | NodeKind::While { .. }
                );
                let new_value = if is_pulse_node && self.fired_this_tick.contains(&slot) {
                    // Pulse fired - re-read body value which may have been updated
                    match &node.kind {
                        NodeKind::Then { body, .. } => self.arena.get_value(*body).clone(),
                        NodeKind::When { input, arms } => {
                            let input_value = self.arena.get_value(*input);
                            if is_skip(input_value) {
                                Value::Skip
                            } else {
                                let mut result = Value::Skip;
                                for (pattern, body_slot) in arms {
                                    if pattern_matches(pattern, input_value) {
                                        result = self.arena.get_value(*body_slot).clone();
                                        break;
                                    }
                                }
                                result
                            }
                        }
                        NodeKind::While { input, pattern, body } => {
                            let input_value = self.arena.get_value(*input);
                            if is_skip(input_value) || !pattern_matches(pattern, input_value) {
                                Value::Skip
                            } else {
                                self.arena.get_value(*body).clone()
                            }
                        }
                        _ => self.compute_node(&node, slot),
                    }
                } else if is_pulse_node {
                    // Pulse didn't fire - stay Skip
                    continue;
                } else {
                    self.compute_node(&node, slot)
                };

                // HOLD: update state
                if let NodeKind::Hold { state, .. } = &node.kind {
                    if !is_skip(&new_value) {
                        let state_old = self.arena.get_value(*state).clone();
                        if state_old != new_value {
                            self.arena.set_value(*state, new_value.clone());
                            // Mark state's subscribers dirty
                            self.add_subscribers_to_heap(&mut dirty_heap, *state);
                        }
                    }
                }

                if old_value != new_value {
                    self.arena.set_value(slot, new_value);
                    self.add_subscribers_to_heap(&mut dirty_heap, slot);
                }
            }
        }
    }

    /// Mark subscribers of a slot as dirty
    fn mark_subscribers_dirty(&self, slot: SlotId) {
        let subs: Vec<SlotId> = self.arena.get_subscribers(slot).copied().collect();
        for sub in subs {
            self.arena.mark_dirty(sub);
        }
    }

    /// Add subscribers to the priority heap (O(log n) per insertion)
    fn add_subscribers_to_heap(&self, heap: &mut BinaryHeap<TopoSlot>, slot: SlotId) {
        for sub in self.arena.get_subscribers(slot).copied() {
            if !self.arena.is_dirty(sub) {
                self.arena.mark_dirty(sub);
                heap.push(TopoSlot {
                    slot: sub,
                    topo_index: self.arena.get_topo_index(sub),
                });
            }
        }
    }

    /// Refresh HOLD states that contain lists with updated Objects (Phase 3.5)
    /// When nested Objects change (e.g., via toggle_all), the HOLD state contains
    /// stale Object VALUES. This refreshes them from the ListAppend.
    /// A4: Uses pre-classified hold_slots for O(k) instead of O(n)
    fn refresh_hold_list_states(&mut self) {
        for &slot in &self.hold_slots {
            if let Some(node) = self.arena.get_node(slot).cloned() {
                if let NodeKind::Hold { state, body, .. } = &node.kind {
                    // Check if state contains a list
                    let state_value = self.arena.get_value(*state);
                    if matches!(state_value, Value::List(_)) {
                        // Find the ListAppend in the body chain
                        // Body is usually THEN, whose body is ListAppend
                        if let Some(body_node) = self.arena.get_node(*body) {
                            let list_append_slot = match &body_node.kind {
                                NodeKind::Then { body: then_body, .. } => Some(*then_body),
                                NodeKind::ListAppend { .. } => Some(*body),
                                _ => None,
                            };

                            if let Some(la_slot) = list_append_slot {
                                // Get the fresh ListAppend value
                                let fresh_list = self.arena.get_value(la_slot).clone();
                                if matches!(&fresh_list, Value::List(_)) {
                                    // Update state and slot if different
                                    if *state_value != fresh_list {
                                        self.arena.set_value(*state, fresh_list.clone());
                                        self.arena.set_value(slot, fresh_list);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Prepare LINK values for this tick (Phase 0)
    /// A4: Uses pre-classified link_slots for O(k) instead of O(n)
    fn prepare_link_values(&mut self) {
        // Reset all LINK values to Skip, then set from active events
        for &slot in &self.link_slots {
            let old_value = self.arena.get_value(slot).clone();
            let new_value = if let Some(event_value) = self.active_events.get(&slot) {
                event_value.clone()
            } else {
                // Reset to Skip if no event this tick
                Value::Skip
            };

            // If value changed, mark dirty and propagate to subscribers
            if old_value != new_value {
                self.arena.set_value(slot, new_value);
                self.arena.mark_dirty(slot);
                // Mark subscribers dirty so Path/Wire nodes propagate the change
                for &sub in self.arena.get_subscribers(slot) {
                    self.arena.mark_dirty(sub);
                }
            }
        }
    }

    /// Instantiate new items for ListAppend nodes whose triggers fired (Phase 1.5)
    /// A4: Uses pre-classified list_append_slots for O(k) instead of O(n)
    fn instantiate_triggered_list_appends(&mut self) {
        // Collect top-level slots for the snapshot logic
        let top_level_slots: std::collections::HashSet<SlotId> =
            self.top_level.values().copied().collect();

        // Collect slots that need instantiation (to avoid borrow issues)
        let mut to_instantiate: Vec<(SlotId, Box<shared::ast::Expr>, HashMap<String, SlotId>)> = Vec::new();

        for &slot in &self.list_append_slots {
            if let Some(node) = self.arena.get_node(slot) {
                if let NodeKind::ListAppend { trigger, item_template, captures, .. } = &node.kind {
                    if let Some(trigger_slot) = trigger {
                        // Check if trigger has a non-Skip value (THEN has fired)
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
            self.instantiate_list_item(list_append_slot, &item_template, &captures, &top_level_slots);

            // Recompute the ListAppend's value IMMEDIATELY so THEN can read it in Phase 2
            if let Some(node) = self.arena.get_node(list_append_slot).cloned() {
                let new_value = self.compute_node(&node, list_append_slot);
                self.arena.set_value(list_append_slot, new_value);
            }

            // Mark subscribers (HOLD) dirty for Phase 3
            self.mark_subscribers_dirty(list_append_slot);
        }
    }

    /// Instantiate a new item for a ListAppend node
    fn instantiate_list_item(
        &mut self,
        list_append_slot: SlotId,
        item_template: &shared::ast::Expr,
        captures: &HashMap<String, SlotId>,
        top_level_slots: &std::collections::HashSet<SlotId>,
    ) {
        // Snapshot captured values, but keep these reactive:
        // 1. Event-LINKs (with Skip value) - for event handling (toggle_all.click)
        // 2. Non-LINK top-level slots - computed values like all_completed
        //
        // Snapshot:
        // - LINKs with data (new_todo_input with submit event) - instance-specific
        // - Local intermediate values
        let mut snapshotted_bindings = HashMap::new();
        let mut new_const_slots = Vec::new();
        for (name, slot) in captures {
            let is_link = self.arena.get_node(*slot)
                .map(|n| matches!(&n.kind, NodeKind::Link { .. }))
                .unwrap_or(false);
            let value = self.arena.get_value(*slot).clone();
            let is_top_level = top_level_slots.contains(slot);

            // Keep reactive if:
            // - It's a LINK with Skip (event source like toggle_all)
            // - OR it's a non-LINK top-level (computed value like all_completed)
            let keep_reactive = if is_link {
                is_skip(&value)  // Only event-LINKs (Skip) stay reactive
            } else {
                is_top_level     // Non-LINK top-levels stay reactive
            };

            if keep_reactive {
                snapshotted_bindings.insert(name.clone(), *slot);
            } else {
                let const_slot = self.arena.alloc();
                self.arena.set_node(const_slot, Node::new(NodeKind::Constant(value.clone())));
                self.arena.set_value(const_slot, value);
                new_const_slots.push(const_slot);
                snapshotted_bindings.insert(name.clone(), const_slot);
            }
        }

        // Add snapshotted slots to topo_order
        for &slot in &new_const_slots {
            self.topo_order.push(slot);
        }

        // Track slot count before compilation to find new slots
        let slot_count_before = self.arena.len();

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

        // Subscribe ListAppend to the instance so it updates when the instance changes
        self.arena.add_subscriber(instance_slot, list_append_slot);

        // Assign incremental topo-indices for all new slots (A3 optimization)
        // This avoids full recompute while ensuring correct evaluation order
        self.arena.assign_incremental_topo_indices(slot_count_before);

        // A4: Register new slots in type indices
        for i in slot_count_before..self.arena.len() {
            let slot = SlotId(i as u32);
            if let Some(node) = self.arena.get_node(slot) {
                match &node.kind {
                    NodeKind::Link { .. } => self.link_slots.push(slot),
                    NodeKind::Then { .. } | NodeKind::When { .. } | NodeKind::While { .. } => {
                        self.pulse_slots.push(slot);
                    }
                    NodeKind::Hold { .. } => self.hold_slots.push(slot),
                    NodeKind::ListAppend { .. } => self.list_append_slots.push(slot),
                    _ => {}
                }
            }
        }
        // Re-sort pulse_slots since new slots may have been added
        self.pulse_slots.sort_by_key(|slot| self.arena.get_topo_index(*slot));

        // Evaluate new slots immediately (before continuing with rest of tick phases)
        // This ensures instance values are ready when ListAppend is evaluated
        for slot in new_const_slots.iter().copied() {
            if let Some(node) = self.arena.get_node(slot).cloned() {
                let new_value = self.compute_node(&node, slot);
                self.arena.set_value(slot, new_value);
            }
        }
        // Evaluate in reverse order: dependencies are allocated after dependents in compile_expr
        // So iterating backwards processes dependencies before dependents
        for i in (slot_count_before..self.arena.len()).rev() {
            let slot = SlotId(i as u32);
            if let Some(node) = self.arena.get_node(slot).cloned() {
                let new_value = self.compute_node(&node, slot);
                self.arena.set_value(slot, new_value);
            }
        }
        // Add to topo_order in forward order (for future ticks)
        for i in slot_count_before..self.arena.len() {
            self.topo_order.push(SlotId(i as u32));
        }
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
                    let mut current = Value::Object(std::sync::Arc::new(inner));
                    // Wrap in outer path parts if any
                    for &part in parts[1..parts.len()-1].iter().rev() {
                        let mut outer: HashMap<String, Value> = HashMap::new();
                        outer.insert(part.to_string(), current);
                        current = Value::Object(std::sync::Arc::new(outer));
                    }
                    current
                } else {
                    let mut current = payload;
                    // Build from innermost to outermost
                    for &part in parts[1..].iter().rev() {
                        let mut inner: HashMap<String, Value> = HashMap::new();
                        inner.insert(part.to_string(), current);
                        current = Value::Object(std::sync::Arc::new(inner));
                    }
                    current
                }
            } else {
                payload
            };

            // Store event for this slot
            self.active_events.insert(slot, event_value);
            self.arena.mark_dirty(slot);

            // Mark subscribers dirty (no to_vec needed - Cell-based dirty flags)
            for &sub in self.arena.get_subscribers(slot) {
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
                Value::Object(std::sync::Arc::new(obj))
            }

            NodeKind::List(items) => {
                let list: Vec<Value> = items
                    .iter()
                    .map(|slot| self.arena.get_value(*slot).clone())
                    .collect();
                Value::List(std::sync::Arc::new(list))
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
                Value::List(std::sync::Arc::new(list))
            }

            NodeKind::ListAppend { instances, .. } => {
                // Return list of values from all instantiated items
                let list: Vec<Value> = instances
                    .iter()
                    .map(|slot| self.arena.get_value(*slot).clone())
                    .collect();
                Value::List(std::sync::Arc::new(list))
            }

            NodeKind::ListClear { list, trigger } => {
                // Only clear when triggered (or if no trigger)
                let should_clear = match trigger {
                    Some(trigger_slot) => !is_skip(self.arena.get_value(*trigger_slot)),
                    None => true,
                };
                if should_clear {
                    ops::list_clear(self.arena.get_value(*list))
                } else {
                    self.arena.get_value(*list).clone()
                }
            }

            NodeKind::ListRemove { list, index, trigger } => {
                // Only remove when triggered (or if no trigger)
                let should_remove = match trigger {
                    Some(trigger_slot) => !is_skip(self.arena.get_value(*trigger_slot)),
                    None => true,
                };
                if should_remove {
                    let list_val = self.arena.get_value(*list);
                    let index_val = self.arena.get_value(*index);
                    if let Value::Int(idx) = index_val {
                        ops::list_remove(list_val, *idx)
                    } else {
                        list_val.clone()
                    }
                } else {
                    self.arena.get_value(*list).clone()
                }
            }

            NodeKind::ListRetain { list, trigger, predicate_template, item_name, captures } => {
                // Only retain when triggered (or if no trigger)
                let should_retain = match trigger {
                    Some(trigger_slot) => !is_skip(self.arena.get_value(*trigger_slot)),
                    None => true,
                };
                if should_retain {
                    let list_val = self.arena.get_value(*list);
                    match list_val {
                        Value::List(items) => {
                            // Evaluate predicate for each item, keep those that return true
                            let retained: Vec<Value> = items
                                .iter()
                                .filter(|item| {
                                    // Create a simple evaluation for the predicate
                                    // For now, we evaluate the predicate by checking item properties
                                    self.evaluate_predicate(predicate_template, item_name, item, captures)
                                })
                                .cloned()
                                .collect();
                            Value::List(std::sync::Arc::new(retained))
                        }
                        _ => list_val.clone(),
                    }
                } else {
                    self.arena.get_value(*list).clone()
                }
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
            "List/clear" => {
                if args.len() >= 1 {
                    ops::list_clear(&args[0])
                } else {
                    Value::Skip
                }
            }
            "List/remove" => {
                if args.len() >= 2 {
                    if let Value::Int(idx) = &args[1] {
                        ops::list_remove(&args[0], *idx)
                    } else {
                        Value::Skip
                    }
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

    /// Evaluate a predicate template for an item
    /// Returns true if the predicate evaluates to Bool(true)
    fn evaluate_predicate(
        &self,
        predicate_template: &shared::ast::Expr,
        item_name: &str,
        item: &Value,
        _captures: &std::collections::HashMap<String, SlotId>,
    ) -> bool {
        // Simple predicate evaluation for item => predicate patterns
        // The predicate is typically item.field |> Bool/not() or similar
        match &predicate_template.kind {
            shared::ast::ExprKind::Pipe(base, method, _args) => {
                // Evaluate base
                let base_val = self.eval_simple_expr(base, item_name, item);
                // Apply method
                match method.as_str() {
                    "Bool/not" => {
                        match base_val {
                            Value::Bool(b) => !b,  // If Bool/not returns true, keep item
                            _ => false,
                        }
                    }
                    _ => false,
                }
            }
            shared::ast::ExprKind::Path(base, field) => {
                let base_val = self.eval_simple_expr(base, item_name, item);
                match ops::get_field(&base_val, field) {
                    Value::Bool(b) => b,
                    _ => false,
                }
            }
            shared::ast::ExprKind::Variable(name) if name == item_name => {
                matches!(item, Value::Bool(true))
            }
            _ => false,
        }
    }

    /// Simple expression evaluation for predicate checking
    fn eval_simple_expr(
        &self,
        expr: &shared::ast::Expr,
        item_name: &str,
        item: &Value,
    ) -> Value {
        match &expr.kind {
            shared::ast::ExprKind::Variable(name) if name == item_name => {
                item.clone()
            }
            shared::ast::ExprKind::Path(base, field) => {
                let base_val = self.eval_simple_expr(base, item_name, item);
                ops::get_field(&base_val, field)
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
