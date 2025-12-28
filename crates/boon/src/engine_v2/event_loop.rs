use std::collections::{BinaryHeap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use super::arena::{Arena, SlotId};
use super::address::Port;
use super::routing::RoutingTable;
use super::message::{FieldId, Payload};
use super::node::{EffectType, NodeKind, RuntimePattern};

/// Entry in the dirty node queue.
#[derive(Clone, Copy, Debug)]
pub struct DirtyEntry {
    pub slot: SlotId,
    pub port: Port,
}

/// Timer event waiting to fire.
#[derive(Clone, Debug)]
pub struct TimerEvent {
    pub deadline_tick: u64,
    pub deadline_ms: f64,
    pub node_id: SlotId,
}

impl PartialEq for TimerEvent {
    fn eq(&self, other: &Self) -> bool {
        self.deadline_tick == other.deadline_tick
    }
}

impl Eq for TimerEvent {}

impl PartialOrd for TimerEvent {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TimerEvent {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Min-heap: smaller deadlines first
        other.deadline_tick.cmp(&self.deadline_tick)
    }
}

/// Central reactive event loop.
pub struct EventLoop {
    pub arena: Arena,
    pub timer_queue: BinaryHeap<TimerEvent>,
    pub dirty_nodes: Vec<DirtyEntry>,
    pub current_tick: u64,
    pub tick_scheduled: AtomicBool,
    pub in_tick: AtomicBool,
    pub pending_ticks: AtomicU32,
    pub routing: RoutingTable,
    /// Inbox: stores pending messages by (target_slot, target_port)
    pub inbox: HashMap<(SlotId, Port), Payload>,
    /// Global route slot for Router/route() - shared across all calls
    pub route_slot: Option<SlotId>,
    /// Visibility conditions for list items: item_slot -> condition_slot
    pub visibility_conditions: HashMap<SlotId, SlotId>,
    /// Timers pending external scheduling (real setTimeout).
    /// Each entry is (node_id, interval_ms).
    pub pending_real_timers: Vec<(SlotId, f64)>,
    /// Pulses nodes that need to emit their next value on the next tick.
    pub pending_pulses: Vec<SlotId>,
    /// Pending route change for bridge to handle (browser URL update).
    pub pending_route_change: Option<Payload>,
}

impl EventLoop {
    pub fn new() -> Self {
        Self {
            arena: Arena::new(),
            timer_queue: BinaryHeap::new(),
            dirty_nodes: Vec::new(),
            current_tick: 0,
            tick_scheduled: AtomicBool::new(false),
            in_tick: AtomicBool::new(false),
            pending_ticks: AtomicU32::new(0),
            routing: RoutingTable::new(),
            inbox: HashMap::new(),
            route_slot: None,
            visibility_conditions: HashMap::new(),
            pending_real_timers: Vec::new(),
            pending_pulses: Vec::new(),
            pending_route_change: None,
        }
    }

    /// Mark a node as dirty (needs reprocessing).
    /// Deduplicates: if the same (slot, port) is already in dirty_nodes, don't add again.
    /// This prevents double-processing when a node receives the same message via multiple paths.
    pub fn mark_dirty(&mut self, slot: SlotId, port: Port) {
        // Check for existing entry with same slot and port
        let already_dirty = self.dirty_nodes.iter().any(|e| e.slot == slot && e.port == port);
        if !already_dirty {
            self.dirty_nodes.push(DirtyEntry { slot, port });
        }
    }

    /// Get the number of nodes in the arena.
    pub fn arena_len(&self) -> usize {
        self.arena.len()
    }

    /// Check if a slot is valid.
    pub fn is_valid(&self, slot: SlotId) -> bool {
        self.arena.is_valid(slot)
    }

    /// Schedule a timer to fire after a delay.
    /// The timer will be added to the pending_real_timers queue for external scheduling.
    pub fn schedule_timer(&mut self, node_id: SlotId, interval_ms: f64) {
        // Don't use tick-based scheduling - use real time
        // Store in pending_real_timers for external scheduler to pick up
        self.pending_real_timers.push((node_id, interval_ms));
    }

    /// Get timers that need external scheduling (with real setTimeout).
    /// Returns (slot_id, interval_ms) pairs. Clears the pending list.
    pub fn take_pending_timers(&mut self) -> Vec<(SlotId, f64)> {
        std::mem::take(&mut self.pending_real_timers)
    }

    /// Fire a timer that was scheduled externally.
    /// Called by the platform when a real setTimeout fires.
    pub fn fire_timer(&mut self, node_id: SlotId) {
        // Mark timer node as dirty via Input(0) to signal it should actually fire.
        // This distinguishes from the initial Port::Output dirty during initialization.
        self.mark_dirty(node_id, Port::Input(0));

        // Re-schedule interval timers
        if let Some(node) = self.arena.get(node_id) {
            if let Some(NodeKind::Timer { interval_ms, active, .. }) = node.kind() {
                if *active {
                    // Schedule next tick via external timer
                    self.pending_real_timers.push((node_id, *interval_ms));
                }
            }
        }
    }

    /// Process timers that are ready to fire (legacy tick-based - not used for real timers).
    fn process_timers(&mut self) {
        // Real timers are handled externally via fire_timer()
        // This is now only for tick-based synthetic timers (testing)
        let current = self.current_tick;
        while let Some(timer) = self.timer_queue.peek() {
            if timer.deadline_tick <= current {
                let timer = self.timer_queue.pop().unwrap();
                // Mark timer node as dirty to emit
                self.mark_dirty(timer.node_id, Port::Output);

                // Re-schedule interval timers
                if let Some(node) = self.arena.get(timer.node_id) {
                    if let Some(NodeKind::Timer { interval_ms, active, .. }) = node.kind() {
                        if *active {
                            self.timer_queue.push(TimerEvent {
                                deadline_tick: current + 1,
                                deadline_ms: *interval_ms,
                                node_id: timer.node_id,
                            });
                        }
                    }
                }
            } else {
                break;
            }
        }
    }

    /// Run one tick of the event loop.
    /// Processes all dirty nodes until quiescence.
    pub fn run_tick(&mut self) {
        self.in_tick.store(true, Ordering::SeqCst);
        self.tick_scheduled.store(false, Ordering::SeqCst);
        self.current_tick += 1;

        // Phase 1: Process timers
        self.process_timers();

        // Phase 2: Process dirty nodes until quiescence
        while !self.dirty_nodes.is_empty() {
            let to_process: Vec<_> = self.dirty_nodes.drain(..).collect();
            for entry in to_process {
                self.process_node(entry);
            }
        }

        // Move pending pulses to dirty_nodes for NEXT tick (not this one)
        // This ensures sequential processing: emit pulse -> process HOLD -> emit next pulse
        if !self.pending_pulses.is_empty() {
            let pulses: Vec<_> = std::mem::take(&mut self.pending_pulses);
            for slot in pulses {
                self.mark_dirty(slot, Port::Output);
            }
        }

        // Phase 3: Finalize scopes (Phase 7 will implement)
        // self.finalize_pending_scopes();

        // Phase 4: Execute effects (Phase 6 will implement)
        // self.execute_pending_effects();

        self.in_tick.store(false, Ordering::SeqCst);

        // Check if more ticks needed
        if self.pending_ticks.swap(0, Ordering::SeqCst) > 0 {
            // Would schedule another tick here
        }
    }

    fn process_node(&mut self, entry: DirtyEntry) {
        // Temporarily disabled to preserve compile-time logs
        // #[cfg(target_arch = "wasm32")]
        // zoon::println!("process_node: entry={:?}", entry);

        // Take the message from inbox (if any) - see §4.6.3
        let msg = self.inbox.remove(&(entry.slot, entry.port));

        let Some(node) = self.arena.get(entry.slot) else {
            #[cfg(target_arch = "wasm32")]
            zoon::println!("  process_node: slot {:?} not found in arena", entry.slot);
            return;
        };

        // Access kind through extension (see §2.2.5.1 lazy allocation)
        let Some(kind) = node.kind().cloned() else {
            #[cfg(target_arch = "wasm32")]
            zoon::println!("  process_node: slot {:?} has no kind", entry.slot);
            return;  // Node has no extension yet (shouldn't happen for dirty nodes)
        };

        #[cfg(target_arch = "wasm32")]
        zoon::println!("  process_node: kind={:?} msg={:?}", std::mem::discriminant(&kind), msg.is_some());

        let output = match &kind {
            NodeKind::Producer { value } => {
                // Producer: if we receive a message, update stored value
                if let Some(payload) = msg.clone() {
                    #[cfg(target_arch = "wasm32")]
                    zoon::println!("Producer {:?} updating value to: {:?}", entry.slot, payload);
                    if let Some(node) = self.arena.get_mut(entry.slot) {
                        if let Some(NodeKind::Producer { value: stored }) = node.kind_mut() {
                            *stored = Some(payload.clone());
                        }
                        // Also update extension's current_value
                        if let Some(ext) = node.extension.as_mut() {
                            ext.current_value = Some(payload.clone());
                        }
                    }
                    Some(payload)
                } else {
                    value.clone()
                }
            }
            NodeKind::Wire { source } => {
                // Wire forwards messages from inbox (reactive) or reads from source (initial)
                let result = msg.clone().or_else(|| {
                    source.and_then(|s| {
                        self.get_current_value(s).cloned()
                    })
                });
                #[cfg(target_arch = "wasm32")]
                zoon::println!("Wire {:?} evaluating: msg={:?} source={:?} -> {:?}", entry.slot, msg, source, result);
                result
            }
            NodeKind::Router { .. } => None, // Router doesn't emit directly

            // Phase 4: Combinators
            NodeKind::Combiner { inputs: _, last_values } => {
                // Update the specific input's cached value and capture the received payload
                let received_value = if let Some(payload) = msg.clone() {
                    let input_idx = entry.port.input_index();
                    if let Some(node) = self.arena.get_mut(entry.slot) {
                        if let Some(NodeKind::Combiner { last_values, .. }) = node.kind_mut() {
                            if input_idx < last_values.len() {
                                last_values[input_idx] = Some(payload.clone());
                            }
                        }
                    }
                    Some(payload)
                } else {
                    None
                };
                // LATEST semantics: emit when ANY input arrives (not waiting for all)
                // This is the key semantic: merge streams, emit on each arrival
                received_value
            }
            NodeKind::Register { stored_value, initial_received, .. } => {
                // HOLD: Update stored value on body input or initial input
                if let Some(payload) = msg.clone() {
                    // Check if this is from initial_input (Port::Input(1))
                    let is_initial_input = entry.port == Port::Input(1);

                    if is_initial_input {
                        // Initial input - always update stored_value (for piped function calls)
                        // Only emit on first reception (for normal HOLD semantics)
                        let should_emit = !*initial_received;

                        #[cfg(target_arch = "wasm32")]
                        zoon::println!("Register {:?}: receiving initial value {:?} (first={})",
                            entry.slot, payload, should_emit);

                        if let Some(node) = self.arena.get_mut(entry.slot) {
                            if let Some(NodeKind::Register { stored_value, initial_received, .. }) = node.kind_mut() {
                                *stored_value = Some(payload.clone());
                                *initial_received = true;
                            }
                        }
                        // Also set current_value for get_current_value() access
                        self.set_current_value(entry.slot, payload.clone());

                        if should_emit {
                            Some(payload)
                        } else {
                            None
                        }
                    } else {
                        // Body input (Port::Input(0))
                        #[cfg(target_arch = "wasm32")]
                        zoon::println!("HOLD_BODY_UPDATE: slot={:?} payload={:?}", entry.slot, payload);
                        // DEBUG: Log HOLD body updates to localStorage
                        #[cfg(target_arch = "wasm32")]
                        {
                            use zoon::{local_storage, WebStorage};
                            let entry_str = format!("HOLD:{}|", entry.slot.index);
                            let existing: String = match local_storage().get("hold_updates") {
                                Some(Ok(s)) => s,
                                _ => String::new(),
                            };
                            let _ = local_storage().insert("hold_updates", &format!("{}{}", existing, entry_str));
                        }

                        match &payload {
                            Payload::Flushed(_) => {
                                // FLUSH + HOLD: Don't store error, propagate it
                                Some(payload)
                            }
                            _ => {
                                // Deep-clone ObjectHandle payloads to break self-reference.
                                // Without cloning, the body's nodes (Extractors, Arithmetic)
                                // become part of the state, creating cycles when reading state.
                                let cloned_payload = self.deep_clone_payload(&payload);

                                // Store new state and emit
                                if let Some(node) = self.arena.get_mut(entry.slot) {
                                    if let Some(NodeKind::Register { stored_value, .. }) = node.kind_mut() {
                                        *stored_value = Some(cloned_payload.clone());
                                    }
                                }
                                Some(cloned_payload)
                            }
                        }
                    }
                } else {
                    // Initial emission
                    stored_value.clone()
                }
            }
            NodeKind::Transformer { body_slot, .. } => {
                // THEN: On input arrival, trigger body re-evaluation and get output
                // Port::Input(0) = main input trigger - re-evaluate body and emit
                // Port::Input(1) = body update - forward reactive updates from body

                if entry.port == Port::Input(1) {
                    // Body update - forward the value directly
                    // This enables reactive function return values (like WHILE) to flow through
                    #[cfg(target_arch = "wasm32")]
                    zoon::println!("Transformer {:?}: forwarding body update {:?}", entry.slot, msg);
                    msg.clone()
                } else if msg.is_some() {
                    // Main input (Port::Input(0)) - re-evaluate body and emit
                    // Re-evaluate all nodes in the body subgraph.
                    // This is crucial because body nodes (Extractors, Arithmetic) may have
                    // cached stale values. We need to process them to read fresh state.
                    if let Some(slot) = *body_slot {
                        // Collect all nodes in the body subgraph
                        let body_nodes = self.collect_body_nodes(slot);

                        // Process each node to update its current_value
                        for node_slot in body_nodes {
                            self.process_node(DirtyEntry { slot: node_slot, port: Port::Output });
                        }
                    }

                    // Now read the freshly computed value
                    let result = body_slot.and_then(|slot| {
                        self.arena.get(slot).and_then(|n| {
                            n.extension.as_ref()
                                .and_then(|ext| ext.current_value.clone())
                        })
                    });
                    result
                } else {
                    None
                }
            }
            NodeKind::PatternMux { current_arm, arms, .. } => {
                // WHEN: Match patterns and emit from first matching arm (one-shot copy)
                // Only Input(0) is used - body slots are not wired for reactive updates

                if entry.port != Port::Input(0) {
                    return; // Ignore non-input ports
                }

                if let Some(payload) = msg {
                    #[cfg(target_arch = "wasm32")]
                    zoon::println!("PatternMux: matching payload={:?}", payload);

                    // Find first matching pattern and its index
                    let matched = arms.iter().enumerate()
                        .find(|(_, (pattern, _))| pattern.matches(&payload));

                    if let Some((idx, (pattern, body_slot))) = matched {
                        #[cfg(target_arch = "wasm32")]
                        zoon::println!("PatternMux: matched arm {} pattern {:?}, body_slot={:?}",
                            idx, std::mem::discriminant(pattern), body_slot);

                        // Update current_arm
                        if let Some(node) = self.arena.get_mut(entry.slot) {
                            if let Some(NodeKind::PatternMux { current_arm, .. }) = node.kind_mut() {
                                *current_arm = Some(idx);
                            }
                        }

                        // Only trigger the body slot with payload for Binding patterns.
                        if matches!(pattern, RuntimePattern::Binding(_)) {
                            self.inbox.insert((*body_slot, Port::Input(0)), payload.clone());
                            self.process_node(DirtyEntry { slot: *body_slot, port: Port::Input(0) });
                        }

                        // Read the body value - filter out Unit (SKIP) values
                        let value = self.get_current_value(*body_slot).cloned();

                        #[cfg(target_arch = "wasm32")]
                        zoon::println!("PatternMux: body value={:?}", value);

                        // SKIP (Unit) means "no emission" - don't forward it
                        match &value {
                            Some(Payload::Unit) => None,
                            _ => value,
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            NodeKind::SwitchedWire { arms, .. } => {
                // WHILE: Find matching pattern and switch to that arm continuously
                // Port::Input(0) = main input that determines which arm is active
                // Port::Input(1+i) = body update from arm i (forward if it's the current arm)
                #[cfg(target_arch = "wasm32")]
                zoon::println!("SwitchedWire evaluating: port={:?} msg={:?} arms={}", entry.port, msg, arms.len());

                // Check if this is a body update (Input(n) where n >= 1)
                let body_arm_index = match entry.port {
                    Port::Input(n) if n >= 1 => Some((n - 1) as usize),
                    _ => None,
                };

                if let Some(arm_index) = body_arm_index {
                    // Message from a body slot - forward if it's the current arm
                    let current_arm = self.arena.get(entry.slot).and_then(|n| {
                        if let Some(NodeKind::SwitchedWire { current_arm, .. }) = n.kind() {
                            *current_arm
                        } else {
                            None
                        }
                    });

                    #[cfg(target_arch = "wasm32")]
                    zoon::println!("  body update: arm_index={} current_arm={:?}", arm_index, current_arm);

                    if current_arm == Some(arm_index) {
                        // This is the active arm - forward its value
                        #[cfg(target_arch = "wasm32")]
                        zoon::println!("  forwarding body value: {:?}", msg);
                        msg.clone()
                    } else {
                        // Not the active arm - ignore
                        None
                    }
                } else if let Some(payload) = msg.clone() {
                    // Main input (Port::Input(0)) - find matching pattern
                    let matching_idx = arms.iter()
                        .position(|(pattern, _)| pattern.matches(&payload));

                    #[cfg(target_arch = "wasm32")]
                    zoon::println!("  matching_idx={:?}", matching_idx);

                    // Update current_arm in the node
                    if let Some(node) = self.arena.get_mut(entry.slot) {
                        if let Some(NodeKind::SwitchedWire { current_arm, .. }) = node.kind_mut() {
                            *current_arm = matching_idx;
                        }
                    }

                    // Emit from the matching arm's body
                    let result = matching_idx.and_then(|idx| {
                        arms.get(idx).and_then(|(_, body_slot)| {
                            #[cfg(target_arch = "wasm32")]
                            zoon::println!("  body_slot={:?}", body_slot);
                            self.get_current_value(*body_slot).cloned()
                        })
                    });

                    #[cfg(target_arch = "wasm32")]
                    zoon::println!("  result={:?}", result);

                    result
                } else {
                    // No input, get current arm value
                    let current_arm = {
                        self.arena.get(entry.slot).and_then(|n| {
                            if let Some(NodeKind::SwitchedWire { current_arm, .. }) = n.kind() {
                                *current_arm
                            } else {
                                None
                            }
                        })
                    };
                    current_arm.and_then(|idx| {
                        arms.get(idx).and_then(|(_, body_slot)| {
                            self.get_current_value(*body_slot).cloned()
                        })
                    })
                }
            }

            // Phase 5: Lists
            NodeKind::Bus { items, .. } => {
                // Bus emits ListHandle pointing to itself
                Some(Payload::ListHandle(entry.slot))
            }

            NodeKind::ListAppender { bus_slot, template_input, template_output, .. } => {
                // When input emits a value, clone the template subgraph (if present) or create a simple Producer
                if let Some(payload) = msg {
                    let bus_slot = *bus_slot;
                    let template_input = *template_input;
                    let template_output = *template_output;

                    #[cfg(target_arch = "wasm32")]
                    zoon::println!("ListAppender: received {:?}, appending to bus {:?}", payload, bus_slot);

                    // Deep-clone objects so they don't share field references
                    let cloned_payload = self.deep_clone_payload(&payload);

                    // Determine the item slot to add to the Bus
                    let item_slot = match (template_input, template_output) {
                        (Some(tmpl_in), Some(tmpl_out)) => {
                            // Has a template - clone the transform subgraph for this item
                            #[cfg(target_arch = "wasm32")]
                            zoon::println!("ListAppender: cloning template {:?} -> {:?}", tmpl_in, tmpl_out);

                            // Create a Producer for the trigger value
                            let source_slot = self.arena.alloc();
                            if let Some(node) = self.arena.get_mut(source_slot) {
                                node.set_kind(NodeKind::Producer { value: Some(cloned_payload.clone()) });
                                node.extension_mut().current_value = Some(cloned_payload);
                            }

                            // Clone the template subgraph, rewiring template_input to source_slot
                            let cloned_output = self.clone_transform_subgraph_runtime(
                                tmpl_in,
                                tmpl_out,
                                source_slot,
                            );

                            #[cfg(target_arch = "wasm32")]
                            zoon::println!("ListAppender: cloned template, output {:?}", cloned_output);

                            // DEBUG: Inspect cloned Router's fields to verify remapping
                            #[cfg(target_arch = "wasm32")]
                            {
                                use zoon::{local_storage, WebStorage};
                                // cloned_output is a Transformer, its body is the Router
                                if let Some(node) = self.arena.get(cloned_output) {
                                    if let Some(NodeKind::Transformer { body_slot: Some(router_slot), .. }) = node.kind() {
                                        if let Some(router_node) = self.arena.get(*router_slot) {
                                            if let Some(NodeKind::Router { fields }) = router_node.kind() {
                                                let completed_id = self.arena.get_field_id("completed");
                                                let completed_slot = completed_id.and_then(|id| fields.get(&id).copied());
                                                let completed_kind = completed_slot.and_then(|s| self.arena.get(s)).and_then(|n| n.kind())
                                                    .map(|k| match k {
                                                        NodeKind::Producer { .. } => "Prod",
                                                        NodeKind::Register { .. } => "Reg",
                                                        NodeKind::Wire { .. } => "Wire",
                                                        _ => "Other",
                                                    });
                                                let entry = format!(
                                                    "CLONED_TODO:out={}:rtr={}:completed={:?}:kind={:?};",
                                                    cloned_output.index,
                                                    router_slot.index,
                                                    completed_slot.map(|s| s.index),
                                                    completed_kind
                                                );
                                                let existing: String = match local_storage().get("cloned_todo_log") {
                                                    Some(Ok(s)) => s,
                                                    _ => String::new(),
                                                };
                                                let _ = local_storage().insert("cloned_todo_log", &format!("{}{}", existing, entry));
                                            }
                                        }
                                    }
                                }
                            }

                            // Mark source_slot dirty so it emits its value to all subscribers.
                            // This is critical for cloned HOLD nodes that have initial_input
                            // pointing to source_slot - they need to receive the initial value
                            // via Port::Input(1) to initialize their stored_value.
                            self.mark_dirty(source_slot, Port::Output);

                            cloned_output
                        }
                        _ => {
                            // No template - just create a simple Producer (old behavior)
                            let item_slot = self.arena.alloc();
                            if let Some(node) = self.arena.get_mut(item_slot) {
                                node.set_kind(NodeKind::Producer { value: Some(cloned_payload.clone()) });
                                node.extension_mut().current_value = Some(cloned_payload);
                            }
                            item_slot
                        }
                    };

                    // Add to the Bus
                    if let Some(bus_node) = self.arena.get_mut(bus_slot) {
                        if let Some(NodeKind::Bus { items, alloc_site, .. }) = bus_node.kind_mut() {
                            let key = alloc_site.allocate();
                            items.push((key, item_slot));
                            #[cfg(target_arch = "wasm32")]
                            zoon::println!("ListAppender: added item {:?} with key {}", item_slot, key);
                        }
                    }

                    // Re-emit the Bus to trigger re-render
                    self.mark_dirty(bus_slot, Port::Input(0));
                }
                None // ListAppender doesn't emit directly
            }

            NodeKind::ListClearer { bus_slot, .. } => {
                // When trigger emits any value, clear all items from the Bus
                if msg.is_some() {
                    #[cfg(target_arch = "wasm32")]
                    zoon::println!("ListClearer: trigger received, clearing bus {:?}", bus_slot);

                    // Clear all items from the Bus
                    if let Some(bus_node) = self.arena.get_mut(*bus_slot) {
                        if let Some(NodeKind::Bus { items, .. }) = bus_node.kind_mut() {
                            items.clear();
                            #[cfg(target_arch = "wasm32")]
                            zoon::println!("ListClearer: cleared all items from bus");
                        }
                    }

                    // Re-emit the Bus to trigger re-render
                    self.mark_dirty(*bus_slot, Port::Input(0));
                }
                None // ListClearer doesn't emit directly
            }

            NodeKind::ListRemoveController { source_bus, triggers, template_input, template_output } => {
                // ListRemoveController handles both:
                // 1. Watching source_bus for new items → clone trigger template
                // 2. Receiving trigger events → remove corresponding item
                let source_bus = *source_bus;
                let template_input = *template_input;
                let template_output = *template_output;
                let existing_items: std::collections::HashSet<_> = triggers.keys().copied().collect();

                // Check if this is a trigger event (Port::Input(1+)) or bus change (Port::Input(0))
                let is_trigger_event = matches!(entry.port, Port::Input(n) if n > 0);

                if is_trigger_event && msg.is_some() {
                    // A trigger fired - find and remove the corresponding item
                    // We match the SPECIFIC port that received the event to find the right trigger
                    let trigger_slot_opt = triggers.iter()
                        .find(|&(_, trig)| {
                            // Check if this trigger routes to this EXACT port
                            self.routing.get_subscribers(*trig).iter()
                                .any(|(s, p)| *s == entry.slot && *p == entry.port)
                        })
                        .map(|(&item, _)| item);

                    if let Some(item_to_remove) = trigger_slot_opt {
                        #[cfg(target_arch = "wasm32")]
                        zoon::println!("ListRemoveController: trigger fired, removing item {:?} from bus {:?}", item_to_remove, source_bus);

                        // Remove the item from the Bus
                        if let Some(bus_node) = self.arena.get_mut(source_bus) {
                            if let Some(NodeKind::Bus { items, .. }) = bus_node.kind_mut() {
                                let original_len = items.len();
                                items.retain(|(_, slot)| *slot != item_to_remove);
                                let new_len = items.len();
                                #[cfg(target_arch = "wasm32")]
                                zoon::println!("ListRemoveController: removed {} items (was {}, now {})", original_len - new_len, original_len, new_len);
                            }
                        }

                        // Re-emit the Bus to trigger re-render
                        self.mark_dirty(source_bus, Port::Input(0));
                    }
                } else {
                    // Bus changed - check for new items that need trigger templates
                    let source_items: Vec<_> = if let Some(node) = self.arena.get(source_bus) {
                        if let Some(NodeKind::Bus { items, .. }) = node.kind() {
                            items.clone()
                        } else {
                            vec![]
                        }
                    } else {
                        vec![]
                    };

                    // Clone trigger template for new items
                    let mut new_triggers = Vec::new();
                    if let (Some(tmpl_in), Some(tmpl_out)) = (template_input, template_output) {
                        for (_key, item_slot) in &source_items {
                            if !existing_items.contains(item_slot) {
                                #[cfg(target_arch = "wasm32")]
                                zoon::println!("ListRemoveController: new item {:?}, cloning trigger template", item_slot);

                                // Clone the trigger template for this item
                                let cloned_trigger = self.clone_transform_subgraph_runtime(
                                    tmpl_in,
                                    tmpl_out,
                                    *item_slot,
                                );

                                // Subscribe to the cloned trigger
                                let port_num = (new_triggers.len() + existing_items.len() + 1) as u8;
                                self.routing.add_route(cloned_trigger, entry.slot, Port::Input(port_num));

                                new_triggers.push((*item_slot, cloned_trigger));

                                #[cfg(target_arch = "wasm32")]
                                zoon::println!("ListRemoveController: created trigger {:?} for item {:?} on port {}", cloned_trigger, item_slot, port_num);
                            }
                        }
                    }

                    // Update triggers map with new items
                    if !new_triggers.is_empty() {
                        if let Some(node) = self.arena.get_mut(entry.slot) {
                            if let Some(NodeKind::ListRemoveController { triggers, .. }) = node.kind_mut() {
                                for (item_slot, trigger_slot) in new_triggers {
                                    triggers.insert(item_slot, trigger_slot);
                                }
                            }
                        }
                    }
                }
                None // ListRemoveController doesn't emit directly
            }

            NodeKind::FilteredView { source_bus, conditions, template_input, template_output } => {
                // FilteredView now handles dynamic items too.
                // When source_bus changes (Port::Input(0)), check for new items and clone conditions.
                let source_bus = *source_bus;
                let template_input = *template_input;
                let template_output = *template_output;
                let existing_items: std::collections::HashSet<_> = conditions.keys().copied().collect();

                // Get current items from source Bus
                let source_items: Vec<_> = if let Some(node) = self.arena.get(source_bus) {
                    if let Some(NodeKind::Bus { items, .. }) = node.kind() {
                        items.clone()
                    } else {
                        vec![]
                    }
                } else {
                    vec![]
                };

                // Find new items that need conditions
                let mut new_conditions = Vec::new();
                // DEBUG: Log FilteredView processing
                #[cfg(target_arch = "wasm32")]
                {
                    use zoon::{local_storage, WebStorage};
                    let entry_str = format!("FV:{},tmpl={},items={},existing={};",
                        entry.slot.index,
                        template_input.is_some() && template_output.is_some(),
                        source_items.len(),
                        existing_items.len());
                    let existing: String = match local_storage().get("fv_log") {
                        Some(Ok(s)) => s,
                        _ => String::new(),
                    };
                    let _ = local_storage().insert("fv_log", &format!("{}{}", existing, entry_str));
                }
                if let (Some(tmpl_in), Some(tmpl_out)) = (template_input, template_output) {
                    for (_key, item_slot) in &source_items {
                        if !existing_items.contains(item_slot) {
                            #[cfg(target_arch = "wasm32")]
                            zoon::println!("FilteredView: new item {:?}, cloning condition template", item_slot);
                            #[cfg(target_arch = "wasm32")]
                            {
                                use zoon::{local_storage, WebStorage};
                                let entry_str = format!("NEW_ITEM:{};", item_slot.index);
                                let existing: String = match local_storage().get("fv_new_items") {
                                    Some(Ok(s)) => s,
                                    _ => String::new(),
                                };
                                let _ = local_storage().insert("fv_new_items", &format!("{}{}", existing, entry_str));
                            }

                            // Clone the condition template for this new item
                            // DEBUG: Log source item structure BEFORE cloning (REPLACE log, not append)
                            #[cfg(target_arch = "wasm32")]
                            {
                                use zoon::{local_storage, WebStorage};
                                let item_kind = self.arena.get(*item_slot).and_then(|n| n.kind());
                                let debug_info = match item_kind {
                                    Some(NodeKind::Transformer { body_slot: Some(bs), .. }) => {
                                        let bk = self.arena.get(*bs).and_then(|n| n.kind());
                                        match bk {
                                            Some(NodeKind::Router { fields }) => {
                                                let completed_id = self.arena.get_field_id("completed");
                                                let cs = completed_id.and_then(|id| fields.get(&id).copied());
                                                let ck = cs.and_then(|s| self.arena.get(s)).and_then(|n| n.kind())
                                                    .map(|k| match k {
                                                        NodeKind::Producer { .. } => "Prod",
                                                        NodeKind::Register { .. } => "Reg",
                                                        _ => "Other",
                                                    });
                                                format!("item={}:Xfmr:body={}:Router:comp={:?}:{:?}",
                                                    item_slot.index, bs.index, cs.map(|s| s.index), ck)
                                            }
                                            Some(k) => format!("item={}:Xfmr:body={}:{:?}", item_slot.index, bs.index, std::mem::discriminant(k)),
                                            None => format!("item={}:Xfmr:body={}:NoKind", item_slot.index, bs.index),
                                        }
                                    }
                                    Some(k) => format!("item={}:{:?}", item_slot.index, std::mem::discriminant(k)),
                                    None => format!("item={}:NoKind", item_slot.index),
                                };
                                // REPLACE, not append - shows only LAST clone
                                let _ = local_storage().insert("fv_last_clone_src", &debug_info);
                            }
                            let cloned_cond = self.clone_transform_subgraph_runtime(
                                tmpl_in,
                                tmpl_out,
                                *item_slot,
                            );

                            // DEBUG: After cloning, check what the cloned Extractor's source resolves to
                            #[cfg(target_arch = "wasm32")]
                            {
                                use zoon::{local_storage, WebStorage};
                                // Helper to format Extractor info
                                let format_ext = |ext_slot: SlotId, src: SlotId, field: u32| -> String {
                                    let field_name = self.arena.get_field_name(field)
                                        .map(|s| s.as_ref().to_string())
                                        .unwrap_or_else(|| format!("#{}", field));
                                    let src_kind = self.arena.get(src).and_then(|n| n.kind());
                                    let body_info = match src_kind {
                                        Some(NodeKind::Transformer { body_slot: Some(bs), .. }) => {
                                            let bs_kind = self.arena.get(*bs).and_then(|n| n.kind());
                                            match bs_kind {
                                                Some(NodeKind::Router { fields }) => {
                                                    let fs = fields.get(&field).copied();
                                                    let fk = fs.and_then(|s| self.arena.get(s)).and_then(|n| n.kind())
                                                        .map(|k| match k {
                                                            NodeKind::Producer { .. } => "Prod",
                                                            NodeKind::Register { .. } => "Reg",
                                                            _ => "Other",
                                                        });
                                                    format!("Rtr:fs={:?}:{:?}", fs.map(|s| s.index), fk)
                                                }
                                                _ => format!("NotRtr:{:?}", bs_kind.map(|k| std::mem::discriminant(k))),
                                            }
                                        }
                                        _ => format!("NotXfmr:{:?}", src_kind.map(|k| std::mem::discriminant(k))),
                                    };
                                    format!("Ext={}:src={}:fld={}:{}", ext_slot.index, src.index, field_name, body_info)
                                };

                                let ext_info = if let Some(node) = self.arena.get(cloned_cond) {
                                    match node.kind() {
                                        // Case 1: BoolNot(Extractor) - for active_list
                                        Some(NodeKind::BoolNot { source: Some(ext_slot), .. }) => {
                                            if let Some(ext_node) = self.arena.get(*ext_slot) {
                                                match ext_node.kind() {
                                                    Some(NodeKind::Extractor { source: Some(src), field, .. }) => {
                                                        format!("BoolNot:{}", format_ext(*ext_slot, *src, *field))
                                                    }
                                                    _ => "BoolNot:NotExt".to_string(),
                                                }
                                            } else { "BoolNot:NoExtNode".to_string() }
                                        }
                                        // Case 2: Direct Extractor - for completed_list
                                        Some(NodeKind::Extractor { source: Some(src), field, .. }) => {
                                            format!("Direct:{}", format_ext(cloned_cond, *src, *field))
                                        }
                                        // Case 3: Wire pointing to something
                                        Some(NodeKind::Wire { source: Some(wire_src) }) => {
                                            if let Some(wire_node) = self.arena.get(*wire_src) {
                                                match wire_node.kind() {
                                                    Some(NodeKind::Extractor { source: Some(src), field, .. }) => {
                                                        format!("Wire->Ext:{}", format_ext(*wire_src, *src, *field))
                                                    }
                                                    _ => format!("Wire->{:?}", wire_node.kind().map(|k| std::mem::discriminant(k))),
                                                }
                                            } else { "Wire->None".to_string() }
                                        }
                                        Some(k) => format!("Other:{:?}", std::mem::discriminant(k)),
                                        None => "NoKind".to_string(),
                                    }
                                } else { "NoCond".to_string() };
                                let _ = local_storage().insert("fv_last_clone_ext", &ext_info);
                            }

                            new_conditions.push((*item_slot, cloned_cond));
                        }
                    }
                }

                // Add new conditions to FilteredView
                if !new_conditions.is_empty() {
                    if let Some(node) = self.arena.get_mut(entry.slot) {
                        if let Some(NodeKind::FilteredView { conditions, .. }) = node.kind_mut() {
                            for (item_slot, cond_slot) in &new_conditions {
                                conditions.insert(*item_slot, *cond_slot);
                                #[cfg(target_arch = "wasm32")]
                                zoon::println!("FilteredView: added condition {:?} for item {:?}", cond_slot, item_slot);
                            }
                        }
                    }

                    // Also update global visibility_conditions for rendering
                    for (item_slot, cond_slot) in &new_conditions {
                        self.visibility_conditions.insert(*item_slot, *cond_slot);
                    }

                    // Subscribe ListCount nodes to the new conditions for reactive updates.
                    // ListCount subscribes to FilteredView at compile time, but new conditions
                    // added at runtime also need to trigger ListCount re-evaluation.
                    let filtered_view_slot = entry.slot;
                    let subscribers: Vec<_> = self.routing.get_subscribers(filtered_view_slot).to_vec();
                    #[cfg(target_arch = "wasm32")]
                    {
                        use zoon::{local_storage, WebStorage};
                        let entry_str = format!("FV_SUBS:{},subs={};", filtered_view_slot.index, subscribers.len());
                        let existing: String = match local_storage().get("fv_subs_log") {
                            Some(Ok(s)) => s,
                            _ => String::new(),
                        };
                        let _ = local_storage().insert("fv_subs_log", &format!("{}{}", existing, entry_str));
                    }
                    for (subscriber_slot, _) in subscribers {
                        // Check if subscriber is a ListCount node
                        if let Some(node) = self.arena.get(subscriber_slot) {
                            let is_list_count = matches!(node.kind(), Some(NodeKind::ListCount { .. }));
                            #[cfg(target_arch = "wasm32")]
                            {
                                use zoon::{local_storage, WebStorage};
                                let kind_name = node.kind().map(|k| std::mem::discriminant(k));
                                let entry_str = format!("SUB_CHECK:{}:is_lc={};", subscriber_slot.index, is_list_count);
                                let existing: String = match local_storage().get("fv_sub_check") {
                                    Some(Ok(s)) => s,
                                    _ => String::new(),
                                };
                                let _ = local_storage().insert("fv_sub_check", &format!("{}{}", existing, entry_str));
                            }
                            if is_list_count {
                                // Add route from each new condition to this ListCount
                                for (_, cond_slot) in &new_conditions {
                                    self.routing.add_route(*cond_slot, subscriber_slot, Port::Input(0));
                                    #[cfg(target_arch = "wasm32")]
                                    zoon::println!("FilteredView: subscribed ListCount {:?} to new condition {:?}",
                                        subscriber_slot, cond_slot);
                                    #[cfg(target_arch = "wasm32")]
                                    {
                                        use zoon::{local_storage, WebStorage};
                                        let entry_str = format!("LC_SUB:{}:{};", subscriber_slot.index, cond_slot.index);
                                        let existing: String = match local_storage().get("lc_subscribed") {
                                            Some(Ok(s)) => s,
                                            _ => String::new(),
                                        };
                                        let _ = local_storage().insert("lc_subscribed", &format!("{}{}", existing, entry_str));
                                    }
                                }
                            }
                        }
                    }

                    // Mark condition slots dirty so they evaluate
                    for (_, cond_slot) in &new_conditions {
                        self.mark_dirty(*cond_slot, Port::Input(0));
                    }
                }

                // Forward the ListHandle from the source bus
                Some(Payload::ListHandle(source_bus))
            }

            NodeKind::ListMapper {
                source_bus,
                output_bus,
                template_input,
                template_output,
                mapped_items,
            } => {
                // When source Bus changes, check for new items and transform them
                let source_bus = *source_bus;
                let output_bus = *output_bus;
                let template_input = *template_input;
                let template_output = *template_output;
                let existing_mapped: std::collections::HashSet<_> = mapped_items.keys().copied().collect();

                // Get current items from source Bus
                let source_items: Vec<_> = if let Some(node) = self.arena.get(source_bus) {
                    if let Some(NodeKind::Bus { items, .. }) = node.kind() {
                        items.clone()
                    } else {
                        vec![]
                    }
                } else {
                    vec![]
                };

                // Find items that were removed (in existing_mapped but no longer in source)
                let source_item_set: std::collections::HashSet<_> = source_items.iter().map(|(_, slot)| *slot).collect();
                let removed_items: Vec<_> = existing_mapped.iter()
                    .filter(|slot| !source_item_set.contains(*slot))
                    .copied()
                    .collect();

                // Remove mapped items from output bus
                if !removed_items.is_empty() {
                    #[cfg(target_arch = "wasm32")]
                    zoon::println!("ListMapper: removing {} items from output bus", removed_items.len());

                    // Get the mapped output slots to remove
                    let slots_to_remove: Vec<_> = {
                        if let Some(node) = self.arena.get(entry.slot) {
                            if let Some(NodeKind::ListMapper { mapped_items, .. }) = node.kind() {
                                removed_items.iter()
                                    .filter_map(|item_slot| mapped_items.get(item_slot).copied())
                                    .collect()
                            } else {
                                vec![]
                            }
                        } else {
                            vec![]
                        }
                    };

                    // Remove from output Bus
                    if let Some(bus_node) = self.arena.get_mut(output_bus) {
                        if let Some(NodeKind::Bus { items, .. }) = bus_node.kind_mut() {
                            items.retain(|(_, slot)| !slots_to_remove.contains(slot));
                        }
                    }

                    // Remove from mapped_items
                    if let Some(node) = self.arena.get_mut(entry.slot) {
                        if let Some(NodeKind::ListMapper { mapped_items, .. }) = node.kind_mut() {
                            for item_slot in &removed_items {
                                mapped_items.remove(item_slot);
                            }
                        }
                    }

                    // Mark output Bus dirty to trigger re-render
                    self.mark_dirty(output_bus, Port::Input(0));
                }

                // Find new items not yet mapped
                let mut new_mappings = Vec::new();
                for (key, item_slot) in &source_items {
                    if !existing_mapped.contains(item_slot) {
                        // Check visibility condition - skip invisible items
                        let visible = if let Some(&cond_slot) = self.visibility_conditions.get(item_slot) {
                            self.get_current_value(cond_slot)
                                .map(|p| matches!(p, Payload::Bool(true)))
                                .unwrap_or(true)
                        } else {
                            true
                        };

                        if !visible {
                            #[cfg(target_arch = "wasm32")]
                            zoon::println!("ListMapper: skipping invisible item {:?} (key {})", item_slot, key);
                            continue;
                        }

                        #[cfg(target_arch = "wasm32")]
                        zoon::println!("ListMapper: new item {:?} (key {}), cloning transform", item_slot, key);

                        // Clone the transform subgraph for this new item
                        let cloned_output = self.clone_transform_subgraph_runtime(
                            template_input,
                            template_output,
                            *item_slot,
                        );

                        new_mappings.push((*key, *item_slot, cloned_output));
                    }
                }

                // Add new items to output Bus and update mapped_items
                if !new_mappings.is_empty() {
                    // First update the mapper's mapped_items
                    if let Some(node) = self.arena.get_mut(entry.slot) {
                        if let Some(NodeKind::ListMapper { mapped_items, .. }) = node.kind_mut() {
                            for (_, item_slot, cloned_output) in &new_mappings {
                                mapped_items.insert(*item_slot, *cloned_output);
                            }
                        }
                    }

                    // Add to output Bus
                    if let Some(bus_node) = self.arena.get_mut(output_bus) {
                        if let Some(NodeKind::Bus { items, alloc_site, .. }) = bus_node.kind_mut() {
                            for (_, _, cloned_output) in &new_mappings {
                                let key = alloc_site.allocate();
                                items.push((key, *cloned_output));
                                #[cfg(target_arch = "wasm32")]
                                zoon::println!("ListMapper: added mapped item {:?} with key {}", cloned_output, key);
                            }
                        }
                    }

                    // Mark output Bus dirty to trigger re-render
                    self.mark_dirty(output_bus, Port::Input(0));
                }

                None // ListMapper doesn't emit directly
            }

            // Phase 6: Timer & Effects
            NodeKind::Timer { .. } => {
                // Timer only emits when explicitly fired via fire_timer().
                // fire_timer() marks dirty with Port::Input(0), so we check for that.
                // Initial dirty (Port::Output) should NOT emit.
                if entry.port == Port::Input(0) {
                    Some(Payload::Unit)
                } else {
                    None
                }
            }
            NodeKind::Pulses { total, current, started } => {
                // Pulses emits 0, 1, 2, ..., total-1 sequentially
                let (emit_value, schedule_more) = if !started {
                    // First pulse - start from 0
                    if let Some(node) = self.arena.get_mut(entry.slot) {
                        if let Some(NodeKind::Pulses { started, .. }) = node.kind_mut() {
                            *started = true;
                        }
                    }
                    if *total > 0 {
                        (Some(Payload::Number(0.0)), *total > 1)
                    } else {
                        (None, false)
                    }
                } else if *current + 1 < *total {
                    // Emit next pulse
                    let next = *current + 1;
                    if let Some(node) = self.arena.get_mut(entry.slot) {
                        if let Some(NodeKind::Pulses { current, .. }) = node.kind_mut() {
                            *current = next;
                        }
                    }
                    (Some(Payload::Number(next as f64)), next + 1 < *total)
                } else {
                    (None, false)
                };

                // Schedule next pulse if there are more
                if schedule_more {
                    self.pending_pulses.push(entry.slot);
                }

                emit_value
            }
            NodeKind::Skip { source, count, skipped, last_skipped_value } => {
                // Skip node - skips first N values, then passes through
                // IMPORTANT: Only process when we have an actual incoming message.
                // The msg=None case is initial processing and should NOT count toward skip.
                if msg.is_none() {
                    #[cfg(target_arch = "wasm32")]
                    zoon::println!("Skip {:?}: msg=None, ignoring initial processing", entry.slot);
                    None
                } else {
                    let source_value = self.get_current_value(*source).cloned();

                    #[cfg(target_arch = "wasm32")]
                    zoon::println!("Skip {:?}: msg={:?} source_value={:?} skipped={} count={} last_skipped={:?}",
                        entry.slot, msg, source_value, *skipped, *count, last_skipped_value);

                    if let Some(value) = source_value {
                        // Check if this is the same value we already skipped
                        // This handles duplicate deliveries of the same value across tick iterations
                        if last_skipped_value.as_ref() == Some(&value) {
                            #[cfg(target_arch = "wasm32")]
                            zoon::println!("Skip {:?}: DUPLICATE of already-skipped value {:?}, ignoring",
                                entry.slot, value);
                            None
                        } else if *skipped < *count {
                            // New value to skip - increment counter, store it, don't emit
                            let new_skipped = *skipped + 1;
                            #[cfg(target_arch = "wasm32")]
                            zoon::println!("Skip {:?}: SKIPPING value {:?} (skipped {} -> {})",
                                entry.slot, value, *skipped, new_skipped);
                            if let Some(node) = self.arena.get_mut(entry.slot) {
                                if let Some(NodeKind::Skip { skipped, last_skipped_value, .. }) = node.kind_mut() {
                                    *skipped = new_skipped;
                                    *last_skipped_value = Some(value);
                                }
                            }
                            None
                        } else {
                            // Done skipping - pass through and clear last_skipped
                            #[cfg(target_arch = "wasm32")]
                            zoon::println!("Skip {:?}: PASSING value {:?}", entry.slot, value);
                            if let Some(node) = self.arena.get_mut(entry.slot) {
                                if let Some(NodeKind::Skip { last_skipped_value, .. }) = node.kind_mut() {
                                    *last_skipped_value = None;
                                }
                            }
                            Some(value)
                        }
                    } else {
                        #[cfg(target_arch = "wasm32")]
                        zoon::println!("Skip {:?}: no source value, returning None", entry.slot);
                        None
                    }
                }
            }
            NodeKind::Accumulator { sum, has_input } => {
                // Accumulator sums incoming numbers
                // Only emit after receiving at least one input (prevents showing 0 before timer fires)
                if let Some(Payload::Number(n)) = msg {
                    let new_sum = sum + n;
                    if let Some(node) = self.arena.get_mut(entry.slot) {
                        if let Some(NodeKind::Accumulator { sum, has_input }) = node.kind_mut() {
                            *sum = new_sum;
                            *has_input = true;
                        }
                    }
                    Some(Payload::Number(new_sum))
                } else if *has_input {
                    // Only emit current sum if we've received at least one input
                    Some(Payload::Number(*sum))
                } else {
                    // No input received yet - don't emit anything
                    None
                }
            }
            NodeKind::Arithmetic { op, left, right, .. } => {
                use crate::engine_v2::node::ArithmeticOp;

                // Always read fresh values from sources using get_current_value()
                // This ensures we get the latest values even when inside HOLD body
                // where the state variable may have changed without triggering a message
                let fresh_left = left.and_then(|s| {
                    self.get_current_value(s).and_then(|p| match p {
                        Payload::Number(n) => Some(*n),
                        _ => None,
                    })
                });
                let fresh_right = right.and_then(|s| {
                    self.get_current_value(s).and_then(|p| match p {
                        Payload::Number(n) => Some(*n),
                        _ => None,
                    })
                });

                #[cfg(target_arch = "wasm32")]
                zoon::println!("Arithmetic {:?}: op={:?} left={:?} right={:?} fresh_left={:?} fresh_right={:?}",
                    entry.slot, op, left, right, fresh_left, fresh_right);

                // Update cached values for future reads via current_value
                if let Some(node) = self.arena.get_mut(entry.slot) {
                    if let Some(NodeKind::Arithmetic { left_value, right_value, .. }) = node.kind_mut() {
                        if fresh_left.is_some() {
                            *left_value = fresh_left;
                        }
                        if fresh_right.is_some() {
                            *right_value = fresh_right;
                        }
                    }
                }

                // Compute result using fresh values
                let result = match op {
                    ArithmeticOp::Negate => {
                        fresh_left.map(|l| Payload::Number(-l))
                    }
                    _ => {
                        if let (Some(l), Some(r)) = (fresh_left, fresh_right) {
                            let result = match op {
                                ArithmeticOp::Add => l + r,
                                ArithmeticOp::Subtract => l - r,
                                ArithmeticOp::Multiply => l * r,
                                ArithmeticOp::Divide => {
                                    if r != 0.0 { l / r } else { f64::NAN }
                                }
                                ArithmeticOp::Negate => unreachable!(),
                            };
                            Some(Payload::Number(result))
                        } else {
                            None
                        }
                    }
                };
                result
            }
            NodeKind::Comparison { op, left, right, left_value, right_value } => {
                use crate::engine_v2::node::ComparisonOp;
                #[cfg(target_arch = "wasm32")]
                zoon::println!("Comparison {:?}: op={:?} msg={:?} port={:?}", entry.slot, op, msg, entry.port);

                // Update the appropriate cached value based on which port received the message
                if let Some(payload) = msg.clone() {
                    let input_idx = entry.port.input_index();
                    if let Some(node) = self.arena.get_mut(entry.slot) {
                        if let Some(NodeKind::Comparison { left_value, right_value, .. }) = node.kind_mut() {
                            if input_idx == 0 {
                                *left_value = Some(payload);
                            } else {
                                *right_value = Some(payload);
                            }
                        }
                    }
                }
                // Get updated values and compute
                let node = self.arena.get(entry.slot);
                let result = node.and_then(|n| n.kind()).and_then(|k| match k {
                    NodeKind::Comparison { op, left_value, right_value, .. } => {
                        if let (Some(l), Some(r)) = (left_value, right_value) {
                            let result = match op {
                                ComparisonOp::Equal => l == r,
                                ComparisonOp::NotEqual => l != r,
                                ComparisonOp::Greater => {
                                    match (l, r) {
                                        (Payload::Number(ln), Payload::Number(rn)) => ln > rn,
                                        _ => false,
                                    }
                                }
                                ComparisonOp::GreaterOrEqual => {
                                    match (l, r) {
                                        (Payload::Number(ln), Payload::Number(rn)) => ln >= rn,
                                        _ => false,
                                    }
                                }
                                ComparisonOp::Less => {
                                    match (l, r) {
                                        (Payload::Number(ln), Payload::Number(rn)) => ln < rn,
                                        _ => false,
                                    }
                                }
                                ComparisonOp::LessOrEqual => {
                                    match (l, r) {
                                        (Payload::Number(ln), Payload::Number(rn)) => ln <= rn,
                                        _ => false,
                                    }
                                }
                            };
                            Some(Payload::Bool(result))
                        } else {
                            None
                        }
                    }
                    _ => None,
                });
                #[cfg(target_arch = "wasm32")]
                zoon::println!("  Comparison result: {:?}", result);
                result
            }
            NodeKind::Effect { effect_type, input } => {
                match effect_type {
                    EffectType::RouterGoTo => {
                        // Update the global route slot with the new route
                        if let Some(payload) = msg.clone() {
                            #[cfg(target_arch = "wasm32")]
                            zoon::println!("Effect RouterGoTo: updating route to {:?}", payload);

                            // Update the route_slot Producer's value
                            if let Some(route_slot) = self.route_slot {
                                if let Some(route_node) = self.arena.get_mut(route_slot) {
                                    if let Some(NodeKind::Producer { value }) = route_node.kind_mut() {
                                        *value = Some(payload.clone());
                                    }
                                    route_node.extension_mut().current_value = Some(payload.clone());
                                }
                                // Mark route_slot dirty to notify subscribers (Router/route())
                                self.mark_dirty(route_slot, Port::Output);
                            }

                            // Store the pending route change for bridge to handle browser URL update
                            self.pending_route_change = Some(payload.clone());

                            // Emit Unit as result (go_to returns [])
                            Some(Payload::Unit)
                        } else {
                            None
                        }
                    }
                    _ => {
                        // Other effects: add to pending effects queue (TBD)
                        None
                    }
                }
            }
            NodeKind::IOPad { .. } => {
                // IOPad forwards events from DOM
                #[cfg(target_arch = "wasm32")]
                zoon::println!("IOPad {:?} processing: msg={:?}", entry.slot, msg);
                let result = msg.clone();
                #[cfg(target_arch = "wasm32")]
                zoon::println!("  IOPad emitting: {:?}", result);
                result
            }

            NodeKind::Extractor { source, field, subscribed_field } => {
                #[cfg(target_arch = "wasm32")]
                zoon::println!("Extractor {:?}: source={:?} field={} subscribed_field={:?} port={:?} msg={:?}", entry.slot, source, field, subscribed_field, entry.port, msg);

                // Log high-slot Extractors for "completed" field to confirm they're processed
                #[cfg(target_arch = "wasm32")]
                {
                    use zoon::{local_storage, WebStorage};
                    let field_name = self.arena.get_field_name(*field)
                        .map(|s| s.as_ref().to_string())
                        .unwrap_or_else(|| format!("#{}", field));
                    if field_name == "completed" && entry.slot.index > 1500 {
                        let entry_str = format!(
                            "EXT_ENTER:{}:src={:?}:sf={:?}:port={:?};",
                            entry.slot.index,
                            source.map(|s| s.index),
                            subscribed_field.map(|s| s.index),
                            entry.port
                        );
                        let existing: String = match local_storage().get("ext_enter_log") {
                            Some(Ok(s)) => s,
                            _ => String::new(),
                        };
                        let _ = local_storage().insert("ext_enter_log", &format!("{}{}", existing, entry_str));
                    }
                }

                // Check if this message is from the subscribed field (Input(1)) vs source (Input(0))
                if entry.port == Port::Input(1) && subscribed_field.is_some() {
                    // Message from subscribed field - read current value from it
                    #[cfg(target_arch = "wasm32")]
                    {
                        let field_name = self.arena.get_field_name(*field);
                        let current_val = subscribed_field.and_then(|fs| self.get_current_value(fs).cloned());
                        zoon::println!("  Extractor {:?}: RECEIVED on Port::Input(1) field={:?} subscribed={:?} current_val={:?}",
                            entry.slot, field_name, subscribed_field, current_val.as_ref().map(|v| std::mem::discriminant(v)));
                    }
                    subscribed_field.and_then(|fs| self.get_current_value(fs).cloned())
                } else {
                    // Extract a field from incoming Router/TaggedObject payload
                    // or read from source's current value
                    let source_payload = msg.clone().or_else(|| {
                        source.and_then(|s| self.get_current_value(s).cloned())
                    });

                    #[cfg(target_arch = "wasm32")]
                    {
                        use zoon::{local_storage, WebStorage};
                        let field_name = self.arena.get_field_name(*field)
                            .map(|s| s.as_ref().to_string())
                            .unwrap_or_else(|| format!("#{}", field));

                        // For cloned Extractors (high slot) or "completed" field, log detailed trace
                        if entry.slot.index > 1500 || field_name == "completed" {
                            let src_kind = source.and_then(|s| self.arena.get(s)).and_then(|n| n.kind());
                            let src_kind_str = match src_kind {
                                Some(NodeKind::Transformer { body_slot, .. }) => format!("Xfmr(body={:?})", body_slot.map(|s| s.index)),
                                Some(NodeKind::Router { fields }) => format!("Rtr({}f)", fields.len()),
                                Some(NodeKind::Wire { source: ws }) => format!("Wire(src={:?})", ws.map(|s| s.index)),
                                Some(k) => format!("{:?}", std::mem::discriminant(k)),
                                None => "None".to_string(),
                            };
                            let pl_str = match source_payload.as_ref() {
                                Some(Payload::ObjectHandle(s)) => format!("ObjH({})", s.index),
                                Some(Payload::TaggedObject { tag, fields }) => format!("TagObj(t={},f={})", tag, fields.index),
                                Some(p) => format!("{:?}", std::mem::discriminant(p)),
                                None => "None".to_string(),
                            };
                            let entry_str = format!(
                                "EXT_TRACE:{}:fld={}:src={:?}:sk={}:pl={};",
                                entry.slot.index,
                                field_name,
                                source.map(|s| s.index),
                                src_kind_str,
                                pl_str
                            );
                            let existing: String = match local_storage().get("ext_trace_log") {
                                Some(Ok(s)) => s,
                                _ => String::new(),
                            };
                            let _ = local_storage().insert("ext_trace_log", &format!("{}{}", existing, entry_str));
                        }
                    }

                    // Helper to resolve field slot from a Router
                    let resolve_field = |arena: &Arena, router_slot: SlotId, field_id: u32| -> Option<SlotId> {
                        arena.get(router_slot)
                            .and_then(|n| n.kind())
                            .and_then(|k| match k {
                                NodeKind::Router { fields } => fields.get(&field_id).copied(),
                                _ => None,
                            })
                    };

                    // Helper to find Router by traversing node structure
                    // Follows Transformer.body_slot and Wire.source chains to find Router
                    fn find_router_slot(arena: &Arena, slot: SlotId, depth: usize, trace: &mut String) -> Option<SlotId> {
                        if depth > 10 { return None; }
                        arena.get(slot).and_then(|node| {
                            match node.kind() {
                                Some(NodeKind::Router { .. }) => {
                                    trace.push_str(&format!("R{}", slot.index));
                                    Some(slot)
                                }
                                Some(NodeKind::Transformer { body_slot: Some(body), .. }) => {
                                    trace.push_str(&format!("T{}->", slot.index));
                                    find_router_slot(arena, *body, depth + 1, trace)
                                }
                                Some(NodeKind::Wire { source: Some(src) }) => {
                                    trace.push_str(&format!("W{}->", slot.index));
                                    find_router_slot(arena, *src, depth + 1, trace)
                                }
                                Some(k) => {
                                    trace.push_str(&format!("?{}({:?})", slot.index, std::mem::discriminant(k)));
                                    None
                                }
                                None => {
                                    trace.push_str(&format!("!{}", slot.index));
                                    None
                                }
                            }
                        })
                    }

                    // Find the field slot so we can subscribe to it
                    let mut field_slot = source_payload.as_ref().and_then(|payload| {
                        match payload {
                            Payload::TaggedObject { fields: fields_slot, .. } => {
                                resolve_field(&self.arena, *fields_slot, *field)
                            }
                            Payload::ObjectHandle(obj_slot) => {
                                resolve_field(&self.arena, *obj_slot, *field)
                            }
                            _ => None,
                        }
                    });

                    // Fallback: if source_payload didn't give us a field, try traversing node structure directly
                    // This handles cloned templates where current_value isn't set yet
                    if field_slot.is_none() {
                        if let Some(src) = source {
                            let mut trace = String::new();
                            let router_result = find_router_slot(&self.arena, *src, 0, &mut trace);
                            if let Some(router_slot) = router_result {
                                field_slot = resolve_field(&self.arena, router_slot, *field);
                                #[cfg(target_arch = "wasm32")]
                                {
                                    let field_name = self.arena.get_field_name(*field);
                                    zoon::println!("Extractor {:?}: FALLBACK found router {:?} field {:?} -> {:?}",
                                        entry.slot, router_slot, field_name, field_slot);
                                }
                            }
                            #[cfg(target_arch = "wasm32")]
                            {
                                use zoon::{local_storage, WebStorage};
                                let field_name = self.arena.get_field_name(*field)
                                    .map(|s| s.as_ref().to_string())
                                    .unwrap_or_else(|| format!("#{}", field));
                                // Only log cloned condition Extractors (high slot) + "completed" field
                                if field_name == "completed" && entry.slot.index > 1500 {
                                    let entry_str = format!(
                                        "CLONED_EXT:{}:src={}:tr=[{}]:rtr={:?}:fs={:?};",
                                        entry.slot.index,
                                        src.index,
                                        trace,
                                        router_result.map(|s| s.index),
                                        field_slot.map(|s| s.index)
                                    );
                                    let existing: String = match local_storage().get("cloned_ext_fb") {
                                        Some(Ok(s)) => s,
                                        _ => String::new(),
                                    };
                                    let _ = local_storage().insert("cloned_ext_fb", &format!("{}{}", existing, entry_str));
                                }
                            }
                        }
                    }

                    #[cfg(target_arch = "wasm32")]
                    {
                        use zoon::{local_storage, WebStorage};
                        // Only log for "completed" field or very high slots
                        let field_name = self.arena.get_field_name(*field)
                            .map(|s| s.as_ref())
                            .unwrap_or("?");
                        if field_name == "completed" || entry.slot.index > 1900 {
                            let entry_str = format!(
                                "DYN_EXT:{}:fld={}:fs={:?};",
                                entry.slot.index,
                                field_name,
                                field_slot.map(|s| s.index)
                            );
                            let existing: String = match local_storage().get("dyn_ext_log") {
                                Some(Ok(s)) => s,
                                _ => String::new(),
                            };
                            let _ = local_storage().insert("dyn_ext_log", &format!("{}{}", existing, entry_str));
                        }
                    }

                    // Subscribe to the field slot for future reactive updates
                    // But only subscribe to Producer or IOPad nodes (actual data sources).
                    // Subscribing to Extractor, Arithmetic, or other computed nodes
                    // can create cycles (E1 -> A -> E2 -> E1) that cause infinite loops.
                    // IOPad nodes handle external events (button presses, input changes) and
                    // must be subscribed to for event propagation to work.
                    // Wire nodes must ALSO be subscribable because LINK converts IOPad->Wire,
                    // and the Extractor still needs to receive events from the (now-Wire) slot.
                    if let Some(fs) = field_slot {
                        // Subscribe to Producer nodes (constants/values), IOPad nodes (events),
                        // Combiner nodes (LATEST), Wire nodes (LINK converts IOPad->Wire),
                        // and Register nodes (HOLD state containers).
                        // Register must be subscribable because fields like `item.completed` are often
                        // HOLD expressions that emit new values on state changes.
                        let fs_kind = self.arena.get(fs).and_then(|n| n.kind().cloned());
                        let is_subscribable = fs_kind.as_ref()
                            .map(|k| matches!(k, NodeKind::Producer { .. } | NodeKind::IOPad { .. } | NodeKind::Combiner { .. } | NodeKind::Wire { .. } | NodeKind::Register { .. }))
                            .unwrap_or(false);

                        #[cfg(target_arch = "wasm32")]
                        {
                            use zoon::{local_storage, WebStorage};
                            let kind_name = match fs_kind.as_ref() {
                                Some(NodeKind::Producer { .. }) => "Prod",
                                Some(NodeKind::Wire { .. }) => "Wire",
                                Some(NodeKind::Router { .. }) => "Rtr",
                                Some(NodeKind::Combiner { .. }) => "Cmb",
                                Some(NodeKind::Register { .. }) => "Reg",
                                Some(NodeKind::IOPad { .. }) => "IO",
                                Some(NodeKind::Extractor { .. }) => "Ext",
                                Some(NodeKind::Transformer { .. }) => "Xfm",
                                Some(NodeKind::BoolNot { .. }) => "BNot",
                                _ => "Oth",
                            };
                            // Log cloned "completed" Extractors specifically
                            let field_name = self.arena.get_field_name(*field)
                                .map(|s| s.as_ref().to_string())
                                .unwrap_or_else(|| format!("#{}", field));
                            if field_name == "completed" && entry.slot.index > 1500 {
                                let entry_str = format!(
                                    "CLONED_SUB:{}:fs={}:k={}:sub={};",
                                    entry.slot.index,
                                    fs.index,
                                    kind_name,
                                    is_subscribable
                                );
                                let existing: String = match local_storage().get("cloned_sub_log") {
                                    Some(Ok(s)) => s,
                                    _ => String::new(),
                                };
                                let _ = local_storage().insert("cloned_sub_log", &format!("{}{}", existing, entry_str));
                            }
                            // Only log high slot numbers (cloned nodes) to reduce noise
                            if entry.slot.index > 1500 {
                                let entry_str = format!(
                                    "DYN_SUB:{}:fs={}:k={}:sub={};",
                                    entry.slot.index,
                                    fs.index,
                                    kind_name,
                                    is_subscribable
                                );
                                let existing: String = match local_storage().get("dyn_sub_log") {
                                    Some(Ok(s)) => s,
                                    _ => String::new(),
                                };
                                let _ = local_storage().insert("dyn_sub_log", &format!("{}{}", existing, entry_str));
                            }
                        }

                        if is_subscribable && *subscribed_field != Some(fs) && fs != entry.slot {
                            #[cfg(target_arch = "wasm32")]
                            {
                                let field_name = self.arena.get_field_name(*field);
                                zoon::println!("  Extractor {:?}: SUBSCRIBING to {:?} field={:?} fs_kind={:?}",
                                    entry.slot, fs, field_name, fs_kind.as_ref().map(|k| std::mem::discriminant(k)));
                            }

                            // Add route from field slot to this Extractor
                            self.routing.add_route(fs, entry.slot, Port::Input(1));

                            // Update subscribed_field
                            if let Some(node) = self.arena.get_mut(entry.slot) {
                                if let Some(NodeKind::Extractor { subscribed_field: sub, .. }) = node.kind_mut() {
                                    *sub = Some(fs);
                                }
                            }
                        }
                    }

                    // Get current value from the field slot
                    field_slot.and_then(|fs| self.get_current_value(fs).cloned())
                }
            }

            // Phase 7d: TextTemplate
            NodeKind::TextTemplate { template, dependencies, .. } => {
                // Re-render template when any dependency changes
                let mut values: Vec<String> = Vec::new();
                for dep in dependencies.iter() {
                    let value = self.get_current_value(*dep)
                        .map(|p| p.to_display_string())
                        .unwrap_or_default();
                    values.push(value);
                }
                // Render template with substitutions
                let mut result = template.clone();
                for (i, value) in values.iter().enumerate() {
                    result = result.replace(&format!("{{{}}}", i), value);
                }
                // Update cache and emit
                let text: std::sync::Arc<str> = result.into();
                if let Some(node) = self.arena.get_mut(entry.slot) {
                    if let Some(NodeKind::TextTemplate { cached, .. }) = node.kind_mut() {
                        *cached = Some(text.clone());
                    }
                }
                Some(Payload::Text(text))
            }

            NodeKind::ListCount { source } => {
                // Count items in source Bus at runtime
                #[cfg(target_arch = "wasm32")]
                zoon::println!("ListCount {:?}: source={:?} msg={:?}", entry.slot, source, msg);
                let count = source
                    .and_then(|s| self.count_list_items(s, 0))
                    .unwrap_or(0.0);
                #[cfg(target_arch = "wasm32")]
                zoon::println!("  ListCount result: {}", count);
                Some(Payload::Number(count))
            }

            NodeKind::ListIsEmpty { source } => {
                // Check if source Bus is empty at runtime
                let count = source
                    .and_then(|s| self.count_list_items(s, 0))
                    .unwrap_or(0.0);
                Some(Payload::Bool(count == 0.0))
            }

            NodeKind::BoolNot { source, .. } => {
                // Get input value (from message or source's current value)
                let input_value = msg.clone().or_else(|| {
                    source.and_then(|s| self.get_current_value(s).cloned())
                });

                #[cfg(target_arch = "wasm32")]
                zoon::println!("BoolNot {:?}: input_value={:?}", entry.slot, input_value);

                // Negate boolean value
                let result = match input_value {
                    Some(Payload::Bool(b)) => Some(!b),
                    Some(Payload::Tag(tag_id)) => {
                        // Check for True/False tags (IDs 0 and 1 typically)
                        // True tag -> false, False tag -> true
                        if tag_id == 0 {
                            Some(false) // True -> false
                        } else if tag_id == 1 {
                            Some(true) // False -> true
                        } else {
                            None
                        }
                    }
                    _ => None,
                };

                // Update cache and emit
                if let Some(b) = result {
                    if let Some(node) = self.arena.get_mut(entry.slot) {
                        if let Some(NodeKind::BoolNot { cached, .. }) = node.kind_mut() {
                            *cached = Some(b);
                        }
                    }
                    Some(Payload::Bool(b))
                } else {
                    // Return cached if available
                    let cached = self.arena.get(entry.slot)
                        .and_then(|n| n.kind())
                        .and_then(|k| match k {
                            NodeKind::BoolNot { cached, .. } => *cached,
                            _ => None,
                        });
                    cached.map(Payload::Bool)
                }
            }

            NodeKind::TextTrim { source } => {
                // Get input text (from message or source's current value)
                let input_value = msg.clone().or_else(|| {
                    source.and_then(|s| self.get_current_value(s).cloned())
                });

                #[cfg(target_arch = "wasm32")]
                zoon::println!("TextTrim {:?}: input_value={:?}", entry.slot, input_value);

                // Trim whitespace from text
                match input_value {
                    Some(Payload::Text(s)) => {
                        let trimmed: std::sync::Arc<str> = s.trim().into();
                        Some(Payload::Text(trimmed))
                    }
                    _ => Some(Payload::Text(std::sync::Arc::from("")))
                }
            }

            NodeKind::TextIsNotEmpty { source } => {
                // Get input text (from message or source's current value)
                let input_value = msg.clone().or_else(|| {
                    source.and_then(|s| self.get_current_value(s).cloned())
                });

                #[cfg(target_arch = "wasm32")]
                zoon::println!("TextIsNotEmpty {:?}: input_value={:?}", entry.slot, input_value);

                // Check if text is not empty
                match input_value {
                    Some(Payload::Text(s)) => {
                        let is_not_empty = !s.is_empty();
                        #[cfg(target_arch = "wasm32")]
                        zoon::println!("  TextIsNotEmpty result: {}", is_not_empty);
                        Some(Payload::Bool(is_not_empty))
                    }
                    _ => Some(Payload::Bool(false))
                }
            }

            #[cfg(test)]
            NodeKind::Probe { .. } => {
                // Probe stores incoming message in its `last` field
                if let Some(payload) = msg.clone() {
                    if let Some(node) = self.arena.get_mut(entry.slot) {
                        if let Some(NodeKind::Probe { last }) = node.kind_mut() {
                            *last = Some(payload);
                        }
                    }
                }
                None // Probe doesn't emit
            }

            // LinkResolver forwards input_element's value and is also processed at clone time
            NodeKind::LinkResolver { input_element, .. } => {
                // Forward the input_element's value (pass-through behavior)
                msg.clone().or_else(|| {
                    self.get_current_value(*input_element).cloned()
                })
            }
        };

        if let Some(payload) = output {
            // Store the value as current_value for get_current_value() access
            self.set_current_value(entry.slot, payload.clone());
            // Deliver to all subscribers
            let subscribers = self.routing.get_subscribers(entry.slot);
            #[cfg(target_arch = "wasm32")]
            if !subscribers.is_empty() {
                zoon::println!("Delivering from {:?} to {} subscribers", entry.slot, subscribers.len());
            }
            // DEBUG: Log emit chain to localStorage
            #[cfg(target_arch = "wasm32")]
            {
                use zoon::{local_storage, WebStorage};
                let kind_name = match &kind {
                    NodeKind::Producer { .. } => "Prod",
                    NodeKind::Wire { .. } => "Wire",
                    NodeKind::Router { .. } => "Rout",
                    NodeKind::Combiner { .. } => "Comb",
                    NodeKind::Register { .. } => "Reg",
                    NodeKind::Transformer { .. } => "Trans",
                    NodeKind::PatternMux { .. } => "PMux",
                    NodeKind::SwitchedWire { .. } => "SW",
                    NodeKind::Bus { .. } => "Bus",
                    NodeKind::ListAppender { .. } => "LApp",
                    NodeKind::ListClearer { .. } => "LClr",
                    NodeKind::ListRemoveController { .. } => "LRmC",
                    NodeKind::FilteredView { .. } => "FV",
                    NodeKind::ListMapper { .. } => "LMap",
                    NodeKind::Timer { .. } => "Tmr",
                    NodeKind::Pulses { .. } => "Pls",
                    NodeKind::Skip { .. } => "Skip",
                    NodeKind::Accumulator { .. } => "Acc",
                    NodeKind::Arithmetic { .. } => "Arith",
                    NodeKind::Comparison { .. } => "Cmp",
                    NodeKind::Effect { .. } => "Eff",
                    NodeKind::IOPad { .. } => "IO",
                    NodeKind::Extractor { .. } => "Ext",
                    NodeKind::TextTemplate { .. } => "TT",
                    NodeKind::ListCount { .. } => "LCnt",
                    NodeKind::ListIsEmpty { .. } => "LIsE",
                    NodeKind::BoolNot { .. } => "BNot",
                    NodeKind::TextTrim { .. } => "Trim",
                    NodeKind::TextIsNotEmpty { .. } => "TIsN",
                    NodeKind::LinkResolver { .. } => "LRes",
                    #[cfg(test)]
                    NodeKind::Probe { .. } => "Prb",
                };
                let subs_str: String = subscribers.iter()
                    .map(|(s, p)| {
                        let port_str = match p {
                            Port::Input(i) => format!("i{}", i),
                            Port::Output => "o".to_string(),
                            Port::Field(f) => format!("f{}", f),
                        };
                        format!("{}:{}", s.index, port_str)
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                let entry_str = format!("[{}:{}->{}]", entry.slot.index, kind_name, subs_str);
                let existing: String = match local_storage().get("emit_chain") {
                    Some(Ok(s)) => s,
                    _ => String::new(),
                };
                let _ = local_storage().insert("emit_chain", &format!("{}{}", existing, entry_str));
            }
            self.deliver_message(entry.slot, payload);
        }
    }

    /// Deliver a message to all subscribers of a source node.
    pub fn deliver_message(&mut self, source: SlotId, payload: Payload) {
        let subscribers: Vec<_> = self.routing
            .get_subscribers(source)
            .to_vec();

        for (target, port) in subscribers {
            #[cfg(target_arch = "wasm32")]
            zoon::println!("  -> delivering to {:?} port {:?}", target, port);
            // Store the payload for the target to consume
            self.inbox.insert((target, port), payload.clone());
            self.mark_dirty(target, port);
        }
    }

    /// Get a field slot from a Router, following Wire chains if needed.
    /// Used by LinkResolver to traverse object paths at runtime.
    pub fn get_field_slot(&self, slot: SlotId, field_id: FieldId) -> Option<SlotId> {
        self.get_field_slot_depth(slot, field_id, 0)
    }

    fn get_field_slot_depth(&self, slot: SlotId, field_id: FieldId, depth: usize) -> Option<SlotId> {
        if depth > 20 {
            return None;
        }
        let node = self.arena.get(slot)?;
        match node.kind()? {
            NodeKind::Router { fields } => fields.get(&field_id).copied(),
            NodeKind::Wire { source: Some(source_slot) } => {
                // Follow wire to find actual Router
                self.get_field_slot_depth(*source_slot, field_id, depth + 1)
            }
            NodeKind::Producer { value: Some(Payload::ObjectHandle(router_slot)) } => {
                // ObjectHandle - look inside the referenced Router
                self.get_field_slot_depth(*router_slot, field_id, depth + 1)
            }
            NodeKind::Producer { value: Some(Payload::TaggedObject { fields: fields_slot, .. }) } => {
                // TaggedObject - look inside its fields Router
                self.get_field_slot_depth(*fields_slot, field_id, depth + 1)
            }
            NodeKind::Transformer { body_slot: Some(body), .. } => {
                // Transformer's value comes from body_slot - follow it
                self.get_field_slot_depth(*body, field_id, depth + 1)
            }
            NodeKind::Register { body_input: Some(body), .. } => {
                // Register (HOLD) can also be traversed via body for structural access
                self.get_field_slot_depth(*body, field_id, depth + 1)
            }
            _ => None,
        }
    }

    /// Get the current value stored in a node.
    /// Checks extension.current_value first, then falls back to Producer's value.
    /// Also follows Wire chains and Extractor field access to find the source value.
    pub fn get_current_value(&self, slot: SlotId) -> Option<&Payload> {
        self.get_current_value_depth(slot, 0)
    }

    fn get_current_value_depth(&self, slot: SlotId, depth: usize) -> Option<&Payload> {
        if depth > 20 {
            return None;
        }
        self.arena.get(slot).and_then(|node| {
            // Check node kind FIRST for types that need special handling
            // Wire: ALWAYS follow source chain (never use cached current_value)
            // This ensures we get fresh values from the source, which is crucial for
            // HOLD's state_wire where the source (Register) updates but Wire doesn't
            if let Some(NodeKind::Wire { source: Some(source_slot) }) = node.kind() {
                return self.get_current_value_depth(*source_slot, depth + 1);
            }
            // Transformer: follow body_slot to get value (similar to Wire)
            // This ensures we get values from cloned templates before they process
            if let Some(NodeKind::Transformer { body_slot: Some(body), .. }) = node.kind() {
                return self.get_current_value_depth(*body, depth + 1);
            }
            // Register (HOLD): ALWAYS use stored_value (the authoritative state)
            if let Some(NodeKind::Register { stored_value: Some(val), .. }) = node.kind() {
                return Some(val);
            }
            // Producer: use the stored value
            if let Some(NodeKind::Producer { value: Some(val) }) = node.kind() {
                return Some(val);
            }
            // For other node types, try extension.current_value
            if let Some(ext) = node.extension.as_ref() {
                if let Some(ref val) = ext.current_value {
                    return Some(val);
                }
            }
            // Handle Extractor - get field from source Router
            if let Some(NodeKind::Extractor { source: Some(source_slot), field, subscribed_field }) = node.kind() {
                // If we have a subscribed field, read from it directly
                if let Some(sub_slot) = subscribed_field {
                    return self.get_current_value_depth(*sub_slot, depth + 1);
                }

                // Try to get router_slot either from source_value or directly from source node
                let mut router_slot: Option<SlotId> = None;

                // First, try to get source_value (ObjectHandle or TaggedObject)
                if let Some(source_value) = self.get_current_value_depth(*source_slot, depth + 1) {
                    router_slot = match source_value {
                        Payload::ObjectHandle(rs) => Some(*rs),
                        Payload::TaggedObject { fields: fields_slot, .. } => Some(*fields_slot),
                        _ => None,
                    };
                }

                // Fallback: if source_value didn't give us a router, check if source IS a Router
                // This handles cases where source is a cloned Router that hasn't processed yet
                if router_slot.is_none() {
                    if let Some(source_node) = self.arena.get(*source_slot) {
                        if let Some(NodeKind::Router { .. }) = source_node.kind() {
                            router_slot = Some(*source_slot);
                        }
                    }
                }

                if let Some(rs) = router_slot {
                    // Get field from Router
                    if let Some(router_node) = self.arena.get(rs) {
                        if let Some(NodeKind::Router { fields }) = router_node.kind() {
                            if let Some(field_slot) = fields.get(field) {
                                return self.get_current_value_depth(*field_slot, depth + 1);
                            }
                        }
                    }
                }
            }
            None
        })
    }

    /// Set the current value for a node.
    pub fn set_current_value(&mut self, slot: SlotId, value: Payload) {
        if let Some(node) = self.arena.get_mut(slot) {
            node.extension_mut().current_value = Some(value);
        }
    }

    /// Collect all nodes in a body subgraph, ordered leaves-first for re-evaluation.
    /// This traverses the node's inputs recursively, skipping wires that point to
    /// external nodes (like state_wire pointing to HOLD).
    fn collect_body_nodes(&self, root_slot: SlotId) -> Vec<SlotId> {
        let mut result = Vec::new();
        let mut visited = std::collections::HashSet::new();
        self.collect_body_nodes_impl(root_slot, &mut result, &mut visited);
        result
    }

    fn collect_body_nodes_impl(
        &self,
        slot: SlotId,
        result: &mut Vec<SlotId>,
        visited: &mut std::collections::HashSet<SlotId>,
    ) {
        if visited.contains(&slot) {
            return;
        }
        visited.insert(slot);

        // Get the node's kind and collect its inputs
        if let Some(node) = self.arena.get(slot) {
            if let Some(kind) = node.kind() {
                match kind {
                    NodeKind::Router { fields } => {
                        // Process each field first
                        for (_, field_slot) in fields {
                            self.collect_body_nodes_impl(*field_slot, result, visited);
                        }
                    }
                    NodeKind::Arithmetic { left, right, .. } => {
                        if let Some(l) = left {
                            self.collect_body_nodes_impl(*l, result, visited);
                        }
                        if let Some(r) = right {
                            self.collect_body_nodes_impl(*r, result, visited);
                        }
                    }
                    NodeKind::Extractor { source, .. } => {
                        // Don't traverse into source - that's outside the body
                        // (e.g., state_wire pointing to HOLD)
                        let _ = source;
                    }
                    NodeKind::Wire { source } => {
                        // Don't traverse into wire source - that's outside the body
                        let _ = source;
                    }
                    NodeKind::Combiner { inputs, .. } => {
                        for inp in inputs {
                            self.collect_body_nodes_impl(*inp, result, visited);
                        }
                    }
                    NodeKind::Producer { .. } => {
                        // Producer is a leaf, no inputs
                    }
                    NodeKind::BoolNot { source, .. } => {
                        // Traverse into source, but only if it's not a Wire or Extractor
                        // (those point outside the body and shouldn't be processed)
                        if let Some(src) = source {
                            if let Some(src_node) = self.arena.get(*src) {
                                match src_node.kind() {
                                    Some(NodeKind::Wire { .. }) | Some(NodeKind::Extractor { .. }) => {
                                        // Don't traverse - these point outside the body
                                        // Just add the source itself so it gets processed
                                        if !visited.contains(src) {
                                            visited.insert(*src);
                                            result.push(*src);
                                        }
                                    }
                                    _ => {
                                        // Traverse into other node types
                                        self.collect_body_nodes_impl(*src, result, visited);
                                    }
                                }
                            }
                        }
                    }
                    NodeKind::Comparison { left, right, .. } => {
                        // Traverse into both operands
                        if let Some(l) = left {
                            self.collect_body_nodes_impl(*l, result, visited);
                        }
                        if let Some(r) = right {
                            self.collect_body_nodes_impl(*r, result, visited);
                        }
                    }
                    NodeKind::TextTrim { source, .. } => {
                        if let Some(src) = source {
                            self.collect_body_nodes_impl(*src, result, visited);
                        }
                    }
                    NodeKind::TextIsNotEmpty { source, .. } => {
                        if let Some(src) = source {
                            self.collect_body_nodes_impl(*src, result, visited);
                        }
                    }
                    _ => {}
                }
            }
        }

        // Add this node after its inputs (leaves first)
        result.push(slot);
    }

    /// Deep-clone a payload, creating new slots for ObjectHandle/Router fields.
    /// This ensures that appended list items have independent field values.
    fn deep_clone_payload(&mut self, payload: &Payload) -> Payload {
        match payload {
            Payload::ObjectHandle(router_slot) => {
                // Clone the Router and its fields
                if let Some(cloned_router) = self.deep_clone_router(*router_slot) {
                    Payload::ObjectHandle(cloned_router)
                } else {
                    payload.clone()
                }
            }
            Payload::TaggedObject { tag, fields } => {
                // Clone the fields Router
                if let Some(cloned_fields) = self.deep_clone_router(*fields) {
                    Payload::TaggedObject { tag: *tag, fields: cloned_fields }
                } else {
                    payload.clone()
                }
            }
            // Other payloads are value types, just clone them
            _ => payload.clone(),
        }
    }

    /// Deep-clone a Router node, creating new Producer slots for each field.
    fn deep_clone_router(&mut self, router_slot: SlotId) -> Option<SlotId> {
        #[cfg(target_arch = "wasm32")]
        zoon::println!("deep_clone_router: cloning router {:?}", router_slot);

        // Get the Router's fields
        let fields = {
            let node = self.arena.get(router_slot)?;
            if let Some(NodeKind::Router { fields }) = node.kind() {
                fields.clone()
            } else {
                return None;
            }
        };

        // Create new Producer slots for each field with current values
        let mut new_fields = std::collections::HashMap::new();
        for (field_id, field_slot) in fields.iter() {
            // Get current value of the field - also check what kind of node it is
            #[cfg(target_arch = "wasm32")]
            {
                let field_node = self.arena.get(*field_slot);
                zoon::println!("deep_clone_router: field {:?} slot {:?} kind {:?}",
                    field_id, field_slot, field_node.and_then(|n| n.kind().cloned()));
                if let Some(node) = field_node {
                    zoon::println!("deep_clone_router: field {:?} extension.current_value {:?}",
                        field_id, node.extension.as_ref().and_then(|e| e.current_value.as_ref()));
                }
            }

            let current_value = self.get_current_value(*field_slot).cloned();

            #[cfg(target_arch = "wasm32")]
            zoon::println!("deep_clone_router: field {:?} resolved value {:?}",
                field_id, current_value);

            // Create a new Producer with this value
            let new_field_slot = self.arena.alloc();
            if let Some(node) = self.arena.get_mut(new_field_slot) {
                let value = current_value.clone().unwrap_or(Payload::Unit);
                node.set_kind(NodeKind::Producer { value: Some(value.clone()) });
                node.extension_mut().current_value = Some(value);
            }
            new_fields.insert(*field_id, new_field_slot);
        }

        // Create a new Router with the cloned fields
        let new_router_slot = self.arena.alloc();
        if let Some(node) = self.arena.get_mut(new_router_slot) {
            node.set_kind(NodeKind::Router { fields: new_fields });
            // Set current_value to ObjectHandle pointing to self
            node.extension_mut().current_value = Some(Payload::ObjectHandle(new_router_slot));
        }

        #[cfg(target_arch = "wasm32")]
        zoon::println!("deep_clone_router: cloned {:?} -> {:?}", router_slot, new_router_slot);

        Some(new_router_slot)
    }

    /// Clone a transform subgraph at runtime for ListMapper.
    /// Similar to Evaluator::clone_transform_subgraph but works with the arena directly.
    pub fn clone_transform_subgraph_runtime(
        &mut self,
        template_input: SlotId,
        template_output: SlotId,
        source_item: SlotId,
    ) -> SlotId {
        #[cfg(target_arch = "wasm32")]
        zoon::println!("clone_transform_subgraph_runtime: input={:?} output={:?} source={:?}",
            template_input, template_output, source_item);
        #[cfg(target_arch = "wasm32")]
        if let Some(node) = self.arena.get(template_output) {
            zoon::println!("  template_output kind: {:?}", node.kind().map(|k| std::mem::discriminant(k)));
        } else {
            zoon::println!("  template_output: NO NODE FOUND");
        }
        #[cfg(target_arch = "wasm32")]
        {
            let kind_desc = if let Some(node) = self.arena.get(source_item) {
                match node.kind() {
                    Some(NodeKind::Producer { value }) => format!("Producer({:?})", value.as_ref().map(|v| std::mem::discriminant(v))),
                    Some(NodeKind::Router { fields }) => format!("Router({}f)", fields.len()),
                    Some(NodeKind::Wire { source }) => format!("Wire(src={:?})", source),
                    Some(NodeKind::Transformer { input, body_slot }) => format!("Transformer(in={:?},body={:?})", input, body_slot),
                    Some(k) => format!("{:?}", std::mem::discriminant(k)),
                    None => "NoKind".to_string(),
                }
            } else {
                "NO_NODE".to_string()
            };
            zoon::println!("  source_item kind: {}", kind_desc);
            // Store in localStorage for debugging
            use zoon::WebStorage;
            let _ = zoon::local_storage().insert("clone_source_item", &format!(
                "ti={} to={} si={} kind={}",
                template_input.index, template_output.index, source_item.index, kind_desc
            ));
        }

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

            // CRITICAL: Never clone external nodes. They should stay as references to originals.
            // External nodes include:
            // 1. IOPads with element_slot set - connected to external DOM elements
            //    (IOPads with element_slot: None are internal template IOPads and SHOULD be cloned)
            // 2. Large Routers (>8 fields) - likely top-level objects like `store`
            //    (Internal objects like `todo_elements` have fewer fields)
            if let Some(node) = self.arena.get(slot) {
                match node.kind() {
                    Some(NodeKind::IOPad { element_slot: Some(_), .. }) => {
                        // Skip IOPads connected to external elements - they're references to originals
                        continue;
                    }
                    // IOPads with element_slot: None are internal template IOPads
                    // They MUST be cloned to ensure each list item has its own unique IOPad
                    // Otherwise, all dynamically-added items share the same event channel
                    Some(NodeKind::IOPad { element_slot: None, .. }) => {
                        // Internal IOPad - proceed with cloning (don't skip)
                    }
                    Some(NodeKind::Router { fields }) if fields.len() > 8 => {
                        // Skip large Routers - they're likely external top-level objects
                        // that should not be cloned (e.g., `store` with many fields)
                        continue;
                    }
                    _ => {}
                }
            }

            to_clone.insert(slot);

            // Find dependencies of this slot
            if let Some(node) = self.arena.get(slot) {
                match node.kind() {
                    // Producer - traverse SlotIds inside the value (TaggedObject, ObjectHandle, ListHandle)
                    Some(NodeKind::Producer { value: Some(payload) }) => {
                        match payload {
                            Payload::TaggedObject { fields, .. } => {
                                to_visit.push(*fields);
                            }
                            Payload::ObjectHandle(slot) => {
                                to_visit.push(*slot);
                            }
                            Payload::ListHandle(slot) => {
                                to_visit.push(*slot);
                            }
                            _ => {}
                        }
                    }
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
                    // LinkResolver - traverse input_element (target_source may be template_input)
                    Some(NodeKind::LinkResolver { input_element, target_source, .. }) => {
                        to_visit.push(*input_element);
                        // Only traverse target_source if it's not template_input
                        // (template_input is handled specially - it's remapped, not cloned)
                        if *target_source != template_input {
                            to_visit.push(*target_source);
                        }
                    }
                    _ => {}
                }
            }

            // Also check routing table for LinkResolver subscribers
            // This ensures deferred LINK connections get cloned with the template
            for (target_slot, _port) in self.routing.get_subscribers(slot) {
                if !to_clone.contains(target_slot) {
                    if let Some(node) = self.arena.get(*target_slot) {
                        if matches!(node.kind(), Some(NodeKind::LinkResolver { .. })) {
                            to_visit.push(*target_slot);
                        }
                    }
                }
            }
        }

        #[cfg(target_arch = "wasm32")]
        zoon::println!("clone_transform_subgraph_runtime: collected {} slots to clone", to_clone.len());

        // Debug: show first few nodes that reference template_input in the template
        #[cfg(target_arch = "wasm32")]
        {
            let mut ref_count = 0;
            for &slot in &to_clone {
                if let Some(node) = self.arena.get(slot) {
                    let refs_template = match node.kind() {
                        Some(NodeKind::Wire { source: Some(src) }) => *src == template_input,
                        Some(NodeKind::Extractor { source: Some(src), .. }) => *src == template_input,
                        Some(NodeKind::Transformer { input: Some(inp), .. }) => *inp == template_input,
                        _ => false,
                    };
                    if refs_template && ref_count < 5 {
                        zoon::println!("  template node {:?} references template_input: {:?}", slot, node.kind().map(|k| std::mem::discriminant(k)));
                        ref_count += 1;
                    }
                }
            }
            zoon::println!("  found {} template nodes referencing template_input", ref_count);
        }

        // If nothing to clone (template_output == template_input), just return source_item
        if to_clone.is_empty() {
            #[cfg(target_arch = "wasm32")]
            zoon::println!("clone_transform_subgraph_runtime: empty subgraph, returning source_item");
            return source_item;
        }

        // Create mapping from old slots to new slots
        // Clone all slots reachable from template_output. External references
        // (slots not in to_clone) will be preserved by remap() via unwrap_or().
        let mut slot_map = std::collections::HashMap::new();
        slot_map.insert(template_input, source_item); // Rewire template_input to source_item

        #[cfg(target_arch = "wasm32")]
        zoon::println!("clone_transform_subgraph_runtime: template_input={} in to_clone={}",
            template_input.index, to_clone.contains(&template_input));

        for &old_slot in &to_clone {
            // Skip template_input - it's already mapped to source_item above
            if old_slot == template_input {
                continue;
            }
            let new_slot = self.arena.alloc();
            slot_map.insert(old_slot, new_slot);
        }

        // Debug: Print a few slot_map entries
        #[cfg(target_arch = "wasm32")]
        {
            let mut entries: Vec<_> = slot_map.iter().take(5).collect();
            entries.sort_by_key(|(k, _)| k.index);
            for (old, new) in entries {
                zoon::println!("  slot_map: {} -> {}", old.index, new.index);
            }
            zoon::println!("  slot_map[{}] = {:?}", template_input.index, slot_map.get(&template_input).map(|s| s.index));
        }

        // Debug: Find nodes that originally reference template_input
        #[cfg(target_arch = "wasm32")]
        {
            let mut refs_template_input = 0;
            for &old_slot in &to_clone {
                if let Some(node) = self.arena.get(old_slot) {
                    let refs = match node.kind() {
                        Some(NodeKind::Wire { source: Some(src) }) => *src == template_input,
                        Some(NodeKind::Transformer { input: Some(inp), .. }) => *inp == template_input,
                        Some(NodeKind::Extractor { source: Some(src), .. }) => *src == template_input,
                        _ => false,
                    };
                    if refs {
                        zoon::println!("  OLD slot {} references template_input ({})", old_slot.index, template_input.index);
                        refs_template_input += 1;
                    }
                }
            }
            zoon::println!("  {} nodes reference template_input", refs_template_input);
        }

        // Clone each node with remapped references
        for &old_slot in &to_clone {
            let new_slot = slot_map[&old_slot];

            if let Some(old_node) = self.arena.get(old_slot) {
                let new_kind = self.remap_node_kind_runtime(old_node.kind(), &slot_map);
                let current_value = old_node.extension.as_ref().and_then(|e| e.current_value.clone());
                let is_router = matches!(old_node.kind(), Some(NodeKind::Router { .. }));

                if let Some(new_node) = self.arena.get_mut(new_slot) {
                    if let Some(kind) = new_kind {
                        new_node.set_kind(kind);
                    }
                    // Copy current value with remapped SlotIds
                    if let Some(cv) = current_value {
                        let remapped_cv = Self::remap_payload(&cv, &slot_map);
                        new_node.extension_mut().current_value = Some(remapped_cv);
                    } else if is_router {
                        // For Routers, set current_value = ObjectHandle(self) so that
                        // Extractors can access their fields via get_current_value.
                        // This is needed because Routers don't produce values in process_node
                        // but Extractors need to traverse through them.
                        new_node.extension_mut().current_value = Some(Payload::ObjectHandle(new_slot));
                    }
                }
            }
        }

        // Setup subscriptions for the cloned nodes
        for &old_slot in &to_clone {
            let new_slot = slot_map[&old_slot];

            if let Some(node) = self.arena.get(new_slot) {
                // Subscribe to inputs based on node kind
                match node.kind().cloned() {
                    Some(NodeKind::Wire { source: Some(src) }) => {
                        self.routing.add_route(src, new_slot, Port::Input(0));
                    }
                    Some(NodeKind::Transformer { input: Some(inp), .. }) => {
                        self.routing.add_route(inp, new_slot, Port::Input(0));
                    }
                    Some(NodeKind::Extractor { source: Some(src), subscribed_field: Some(sub), .. }) => {
                        self.routing.add_route(src, new_slot, Port::Input(0));
                        // Also add route from subscribed IOPad to receive events
                        self.routing.add_route(sub, new_slot, Port::Input(1));
                    }
                    Some(NodeKind::Extractor { source: Some(src), subscribed_field: None, .. }) => {
                        self.routing.add_route(src, new_slot, Port::Input(0));
                    }
                    Some(NodeKind::Combiner { inputs, .. }) => {
                        for (i, inp) in inputs.iter().enumerate() {
                            self.routing.add_route(*inp, new_slot, Port::Input(i as u8));
                        }
                    }
                    Some(NodeKind::TextTemplate { dependencies, .. }) => {
                        for (i, dep) in dependencies.iter().enumerate() {
                            self.routing.add_route(*dep, new_slot, Port::Input(i as u8));
                        }
                    }
                    Some(NodeKind::Arithmetic { left: Some(l), right: Some(r), .. }) => {
                        self.routing.add_route(l, new_slot, Port::Input(0));
                        self.routing.add_route(r, new_slot, Port::Input(1));
                    }
                    Some(NodeKind::Comparison { left: Some(l), right: Some(r), .. }) => {
                        self.routing.add_route(l, new_slot, Port::Input(0));
                        self.routing.add_route(r, new_slot, Port::Input(1));
                    }
                    Some(NodeKind::BoolNot { source: Some(src), .. }) => {
                        self.routing.add_route(src, new_slot, Port::Input(0));
                    }
                    // Register (HOLD) - subscribe to body and initial inputs
                    Some(NodeKind::Register { body_input, initial_input, .. }) => {
                        if let Some(body) = body_input {
                            self.routing.add_route(body, new_slot, Port::Input(0));
                        }
                        if let Some(init) = initial_input {
                            self.routing.add_route(init, new_slot, Port::Input(1));
                        }
                    }
                    // SwitchedWire (WHILE) - subscribe to input
                    Some(NodeKind::SwitchedWire { input: Some(inp), .. }) => {
                        self.routing.add_route(inp, new_slot, Port::Input(0));
                    }
                    // PatternMux (WHEN) - subscribe to input
                    Some(NodeKind::PatternMux { input: Some(inp), .. }) => {
                        self.routing.add_route(inp, new_slot, Port::Input(0));
                    }
                    // Effect - subscribe to input
                    Some(NodeKind::Effect { input: Some(inp), .. }) => {
                        self.routing.add_route(inp, new_slot, Port::Input(0));
                    }
                    // IOPad - subscribe to element_slot
                    Some(NodeKind::IOPad { element_slot: Some(el), .. }) => {
                        self.routing.add_route(el, new_slot, Port::Input(0));
                    }
                    // Router - subscribe to all field slots (for propagation)
                    Some(NodeKind::Router { fields }) => {
                        for (field_id, field_slot) in &fields {
                            self.routing.add_route(*field_slot, new_slot, Port::Field(*field_id));
                        }
                    }
                    _ => {}
                }
            }
        }

        // Process LinkResolver nodes - resolve deferred LINK connections
        // This must happen after cloning/remapping so target_source points to the actual item
        let mut link_resolver_count = 0;
        let mut link_resolver_resolved = 0;
        let mut link_resolver_failed = 0;
        let mut debug_details = String::new();
        for &old_slot in &to_clone {
            let new_slot = slot_map[&old_slot];
            // Also check OLD slot for LinkResolver to see original values
            let old_target_info = if let Some(old_node) = self.arena.get(old_slot) {
                if let Some(NodeKind::LinkResolver { target_source: old_ts, .. }) = old_node.kind() {
                    let old_type = if let Some(n) = self.arena.get(*old_ts) {
                        match n.kind() {
                            Some(NodeKind::Wire { source }) => format!("Wire(src={:?})", source),
                            Some(NodeKind::Transformer { input, body_slot }) =>
                                format!("Transformer(in={:?},body={:?})", input, body_slot),
                            Some(k) => format!("{:?}", std::mem::discriminant(k)),
                            None => "NoKind".to_string(),
                        }
                    } else {
                        "NoNode".to_string()
                    };
                    Some((*old_ts, old_type))
                } else { None }
            } else { None };

            if let Some(node) = self.arena.get(new_slot) {
                if let Some(NodeKind::LinkResolver { input_element, target_source, target_path }) = node.kind().cloned() {
                    link_resolver_count += 1;

                    // Debug: show target_source node type
                    let target_node_type = if let Some(tgt_node) = self.arena.get(target_source) {
                        match tgt_node.kind() {
                            Some(NodeKind::Wire { source }) => format!("Wire(src={:?})", source),
                            Some(NodeKind::Transformer { input, body_slot }) =>
                                format!("Transformer(in={:?},body={:?})", input, body_slot),
                            Some(k) => format!("{:?}", std::mem::discriminant(k)),
                            None => "NoKind".to_string(),
                        }
                    } else {
                        "None".to_string()
                    };

                    #[cfg(target_arch = "wasm32")]
                    zoon::println!("Processing LinkResolver #{}: input_element={:?} target_source={:?}({}) path_len={} old={:?}",
                        link_resolver_count, input_element, target_source, target_node_type, target_path.len(), old_target_info);

                    // Follow target_path from target_source to find the IOPad
                    let mut current = target_source;
                    let mut resolved = true;
                    let mut step = 0;

                    for field_id in &target_path {
                        step += 1;
                        // Debug: show current node type before get_field_slot
                        let curr_node_type = if let Some(curr_node) = self.arena.get(current) {
                            match curr_node.kind() {
                                Some(NodeKind::Router { fields }) => format!("Router({}fields)", fields.len()),
                                Some(NodeKind::Wire { source }) => format!("Wire(src={:?})", source),
                                Some(NodeKind::Producer { value }) => format!("Producer({:?})", value.as_ref().map(|v| std::mem::discriminant(v))),
                                Some(k) => format!("{:?}", std::mem::discriminant(k)),
                                None => "NoKind".to_string(),
                            }
                        } else {
                            "NoNode".to_string()
                        };

                        #[cfg(target_arch = "wasm32")]
                        zoon::println!("  Step {}: get_field_slot({:?}={}, field={:?})",
                            step, current, curr_node_type, field_id);

                        if let Some(field_slot) = self.get_field_slot(current, *field_id) {
                            current = field_slot;
                        } else {
                            #[cfg(target_arch = "wasm32")]
                            zoon::println!("  LinkResolver: FAILED at step {} - field {:?} not found in {:?}({})",
                                step, field_id, current, curr_node_type);
                            debug_details.push_str(&format!("R{}:{}->{}@{};",
                                link_resolver_count,
                                old_target_info.as_ref().map(|(_, t)| t.as_str()).unwrap_or("?"),
                                target_node_type,
                                step));
                            resolved = false;
                            link_resolver_failed += 1;
                            break;
                        }
                    }

                    if resolved {
                        link_resolver_resolved += 1;
                        // Convert the target IOPad to a Wire pointing to input_element
                        #[cfg(target_arch = "wasm32")]
                        zoon::println!("  LinkResolver: resolved target={:?}, converting to Wire -> {:?}", current, input_element);

                        if let Some(target_node) = self.arena.get_mut(current) {
                            target_node.set_kind(NodeKind::Wire { source: Some(input_element) });
                        }
                        // IMPORTANT: Add routing so events propagate from input_element to this Wire
                        self.routing.add_route(input_element, current, Port::Input(0));

                        // CRITICAL: Mark the Wire dirty so it emits its value.
                        // This triggers the Extractor chain that was waiting for this LINK
                        // to be established (e.g., todo_elements.todo_checkbox.event.click).
                        // Without this, Extractors processed before LinkResolver would see
                        // None and never re-subscribe after the Wire is connected.
                        self.mark_dirty(current, Port::Input(0));
                    }
                }
            }
        }
        #[cfg(target_arch = "wasm32")]
        zoon::println!("clone_transform_subgraph_runtime: processed {} LinkResolvers", link_resolver_count);

        // Debug: write to localStorage to verify this code path runs
        #[cfg(target_arch = "wasm32")]
        {
            use zoon::WebStorage;
            let _ = zoon::local_storage().insert("link_resolver_debug", &format!(
                "cloned={} resolvers={} ok={} fail={} details={}",
                to_clone.len(),
                link_resolver_count,
                link_resolver_resolved,
                link_resolver_failed,
                debug_details
            ));
        }

        // Trigger evaluation by marking ALL cloned Wire nodes dirty
        // This ensures the cloned subgraph gets evaluated from source_item
        let mut marked_count = 0;

        for &old_slot in &to_clone {
            let new_slot = slot_map[&old_slot];
            if let Some(node) = self.arena.get(new_slot) {
                // Mark entry nodes (Wire/Extractor) dirty - these read from source_item
                // Don't mark TextTemplate directly; let dependency propagation trigger it
                // after its dependencies (Wires) have been evaluated
                match node.kind() {
                    Some(NodeKind::Wire { .. }) |
                    Some(NodeKind::Transformer { .. }) |
                    Some(NodeKind::Extractor { .. }) => {
                        self.mark_dirty(new_slot, Port::Input(0));
                        marked_count += 1;
                    }
                    _ => {}
                }
            }
        }
        #[cfg(target_arch = "wasm32")]
        zoon::println!("clone_transform_subgraph_runtime: marked {} entry nodes dirty", marked_count);

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
            // Other payloads don't contain SlotIds
            other => other.clone(),
        }
    }

    /// Remap a NodeKind's slot references using the given mapping (runtime version).
    fn remap_node_kind_runtime(
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
                let new_fields: std::collections::HashMap<_, _> = fields.iter()
                    .map(|(k, v)| {
                        let remapped = remap(v);
                        // Log if field slot wasn't remapped (original = remapped means not in slot_map)
                        #[cfg(target_arch = "wasm32")]
                        if *v == remapped {
                            // Get field name
                            let field_name = self.arena.get_field_name(*k)
                                .map(|s| s.as_ref().to_string())
                                .unwrap_or_else(|| format!("#{}", k));
                            // Get slot kind
                            let slot_kind = self.arena.get(*v)
                                .and_then(|n| n.kind())
                                .map(|k| match k {
                                    NodeKind::Producer { .. } => "Prod",
                                    NodeKind::Wire { .. } => "Wire",
                                    NodeKind::Register { .. } => "Reg",
                                    NodeKind::Router { .. } => "Rtr",
                                    _ => "Other",
                                })
                                .unwrap_or("None");
                            zoon::println!("REMAP_ROUTER_FIELD: '{}' slot {} ({}) NOT in slot_map, keeping original",
                                field_name, v.index, slot_kind);
                        }
                        (*k, remapped)
                    })
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
            // Extractor - remap source, reset subscribed_field
            // CRITICAL: We reset subscribed_field to None because the subscribed slot may be
            // outside the cloned subgraph. The cloned Extractor will re-establish subscription
            // when it processes and finds its new field slot.
            Some(NodeKind::Extractor { source, field, .. }) => {
                Some(NodeKind::Extractor {
                    source: remap_opt(source),
                    field: *field,
                    subscribed_field: None,
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
            // LinkResolver - remap both input_element and target_source
            Some(NodeKind::LinkResolver { input_element, target_source, target_path }) => {
                Some(NodeKind::LinkResolver {
                    input_element: remap(input_element),
                    target_source: remap(target_source),
                    target_path: target_path.clone(),
                })
            }
            _ => kind.cloned()
        }
    }

    /// Count items in a Bus, following Wire chains.
    /// Respects visibility conditions from FilteredView nodes.
    pub fn count_list_items(&self, slot: SlotId, depth: usize) -> Option<f64> {
        self.count_list_items_with_conditions(slot, depth, None)
    }

    /// Count items with optional per-slot visibility conditions.
    fn count_list_items_with_conditions(
        &self,
        slot: SlotId,
        depth: usize,
        conditions: Option<&std::collections::HashMap<SlotId, SlotId>>,
    ) -> Option<f64> {
        if depth > 10 {
            return Some(0.0);
        }
        let node = self.arena.get(slot)?;
        match node.kind()? {
            NodeKind::FilteredView { source_bus, conditions: view_conditions, .. } => {
                // Use this view's conditions when counting the source bus
                self.count_list_items_with_conditions(*source_bus, depth + 1, Some(view_conditions))
            }
            NodeKind::Bus { items, .. } => {
                // Count items, respecting visibility conditions from FilteredView ONLY
                // NOTE: Do NOT check global visibility_conditions here - those are for rendering.
                // List/count should only respect conditions passed from FilteredView chain.
                let mut count = 0.0;
                #[cfg(target_arch = "wasm32")]
                zoon::println!("count_list_items Bus {:?}: items={:?} has_conditions={}",
                    slot, items.len(), conditions.is_some());
                for (_key, item_slot) in items {
                    // Check visibility ONLY if conditions were passed from a FilteredView
                    let visible = if let Some(conds) = conditions {
                        let has_cond = conds.contains_key(item_slot);
                        let vis = self.is_item_visible_with_conditions(*item_slot, conds);
                        #[cfg(target_arch = "wasm32")]
                        zoon::println!("  item {:?}: has_cond_in_map={} visible={}", item_slot, has_cond, vis);
                        vis
                    } else {
                        // No FilteredView conditions - count ALL items unconditionally
                        // (global visibility_conditions are for rendering only, not counting)
                        true
                    };
                    if visible {
                        count += 1.0;
                    }
                }
                Some(count)
            }
            NodeKind::Wire { source: Some(source) } => {
                self.count_list_items_with_conditions(*source, depth + 1, conditions)
            }
            NodeKind::Wire { source: None } => {
                // Wire source not connected yet - check current_value for ListHandle
                node.extension.as_ref()
                    .and_then(|ext| ext.current_value.as_ref())
                    .and_then(|payload| match payload {
                        Payload::ListHandle(list_slot) => {
                            self.count_list_items_with_conditions(*list_slot, depth + 1, conditions)
                        }
                        _ => Some(0.0),
                    })
            }
            NodeKind::Register { stored_value, .. } => {
                // HOLD - check stored_value for ListHandle
                match stored_value {
                    Some(Payload::ListHandle(list_slot)) => {
                        self.count_list_items_with_conditions(*list_slot, depth + 1, conditions)
                    }
                    _ => Some(0.0),
                }
            }
            _ => {
                // Check current_value for ListHandle
                node.extension.as_ref()
                    .and_then(|ext| ext.current_value.as_ref())
                    .and_then(|payload| match payload {
                        Payload::ListHandle(list_slot) => {
                            self.count_list_items_with_conditions(*list_slot, depth + 1, conditions)
                        }
                        _ => Some(0.0),
                    })
            }
        }
    }

    /// Check if an item passes its visibility condition (if any).
    fn is_item_visible_with_conditions(
        &self,
        item_slot: SlotId,
        conditions: &std::collections::HashMap<SlotId, SlotId>,
    ) -> bool {
        if let Some(&cond_slot) = conditions.get(&item_slot) {
            // Get the current value of the condition
            let cond_value = self.get_current_value(cond_slot);
            #[cfg(target_arch = "wasm32")]
            zoon::println!("    is_item_visible: item={:?} cond_slot={:?} cond_value={:?}",
                item_slot, cond_slot, cond_value);
            if let Some(cond_value) = cond_value {
                match cond_value {
                    Payload::Bool(b) => *b,
                    // If not a bool, consider visible
                    _ => true,
                }
            } else {
                // No value yet, consider visible
                true
            }
        } else {
            // No visibility condition for this item in the FilteredView.
            // This happens when the item was added dynamically (via List/append)
            // AFTER the FilteredView was created.
            // Default to NOT visible - the item doesn't match the filter since
            // we can't evaluate the filter expression for it.
            // This ensures dynamically added items are correctly excluded from
            // filtered counts (e.g., completed_todos_count won't wrongly count
            // newly added incomplete todos as completed).
            false
        }
    }
}

impl Default for EventLoop {
    fn default() -> Self {
        Self::new()
    }
}

// Snapshot support - requires the snapshot module
#[cfg(feature = "cli")]
mod snapshot_support {
    use super::*;
    use crate::engine_v2::snapshot::{GraphSnapshot, SerializedPayload};
    use crate::engine_v2::address::{SourceId, ScopeId};

    impl EventLoop {
        /// Create a snapshot of all persisted state.
        pub fn create_snapshot(&self) -> GraphSnapshot {
            let mut snapshot = GraphSnapshot::new();
            snapshot.copy_intern_tables(&self.arena);

            // Collect values from Register nodes (HOLD state)
            for i in 0..self.arena.len() {
                let slot = SlotId { index: i as u32, generation: 0 };
                if !self.arena.is_valid(slot) {
                    continue;
                }
                if let Some(node) = self.arena.get(slot) {
                    if let Some(NodeKind::Register { stored_value, .. }) = node.kind() {
                        if let Some(value) = stored_value {
                            // Use slot index as a simple persistence key for now
                            // Full persistence would use SourceId/ScopeId from addresses
                            let source_id = SourceId { stable_id: slot.index as u64, parse_order: 0 };
                            let scope_id = ScopeId(0);
                            snapshot.store(source_id, scope_id, value, &self.arena);
                        }
                    }
                }
            }
            snapshot
        }

        /// Restore state from a snapshot.
        pub fn restore_snapshot(&mut self, snapshot: &GraphSnapshot) {
            // First pass: collect which slots need restoration and deserialize values
            let mut restorations: Vec<(SlotId, Payload)> = Vec::new();

            for i in 0..self.arena.len() {
                let slot = SlotId { index: i as u32, generation: 0 };
                if !self.arena.is_valid(slot) {
                    continue;
                }
                if let Some(node) = self.arena.get(slot) {
                    if matches!(node.kind(), Some(NodeKind::Register { .. })) {
                        let source_id = SourceId { stable_id: slot.index as u64, parse_order: 0 };
                        let scope_id = ScopeId(0);
                        if let Some(serialized) = snapshot.retrieve(source_id, scope_id) {
                            if let Some(payload) = deserialize_payload(serialized) {
                                restorations.push((slot, payload));
                            }
                        }
                    }
                }
            }

            // Second pass: apply the restorations
            for (slot, payload) in restorations {
                if let Some(node) = self.arena.get_mut(slot) {
                    if let Some(NodeKind::Register { stored_value, .. }) = node.kind_mut() {
                        *stored_value = Some(payload);
                    }
                }
            }
        }
    }

    fn deserialize_payload(serialized: &SerializedPayload) -> Option<Payload> {
        Some(match serialized {
            SerializedPayload::Number(n) => Payload::Number(*n),
            SerializedPayload::Text(s) => Payload::Text(s.as_str().into()),
            SerializedPayload::Bool(b) => Payload::Bool(*b),
            SerializedPayload::Unit => Payload::Unit,
            SerializedPayload::Tag(t) => Payload::Tag(*t),
            // Lists and objects not yet fully supported for restoration
            // TODO: Implement proper List/Object restoration with arena reconstruction
            SerializedPayload::List(_) => return None,
            SerializedPayload::Object(_) => return None,
            // TaggedObject fields cannot be restored without arena slot reconstruction
            // For now, return just the Tag (loses structure but prevents panic)
            // TODO: Implement proper TaggedObject restoration
            SerializedPayload::TaggedObject { tag, fields: _ } => Payload::Tag(*tag),
        })
    }
}

/// CLI-specific methods for JSON expansion.
#[cfg(feature = "cli")]
impl EventLoop {
    /// Expand a payload to JSON, resolving ListHandle and ObjectHandle.
    pub fn expand_payload_to_json(&self, payload: &Payload) -> serde_json::Value {
        self.expand_payload_to_json_depth(payload, 0)
    }

    fn expand_payload_to_json_depth(&self, payload: &Payload, depth: usize) -> serde_json::Value {
        use serde_json::json;

        if depth > 10 {
            return json!("[max depth]");
        }

        match payload {
            Payload::Number(n) => json!(n),
            Payload::Text(s) => json!(s.as_ref()),
            Payload::Bool(b) => json!(b),
            Payload::Unit => json!(null),
            Payload::Tag(t) => {
                // Try to resolve tag name
                if let Some(name) = self.arena.get_tag_name(*t) {
                    json!(name.as_ref())
                } else {
                    json!(format!("Tag({})", t))
                }
            }
            Payload::TaggedObject { tag, fields } => {
                let mut obj = serde_json::Map::new();
                // Add tag name
                if let Some(name) = self.arena.get_tag_name(*tag) {
                    obj.insert("_tag".to_string(), json!(name.as_ref()));
                }
                // Expand fields from Router
                if let Some(node) = self.arena.get(*fields) {
                    if let Some(NodeKind::Router { fields: field_map }) = node.kind() {
                        for (field_id, field_slot) in field_map {
                            if let Some(name) = self.arena.get_field_name(*field_id) {
                                if let Some(val) = self.get_current_value(*field_slot) {
                                    obj.insert(
                                        name.to_string(),
                                        self.expand_payload_to_json_depth(val, depth + 1)
                                    );
                                }
                            }
                        }
                    }
                }
                serde_json::Value::Object(obj)
            }
            Payload::ListHandle(bus_slot) => {
                // Expand list items from Bus, respecting visibility conditions
                let mut items = vec![];
                if let Some(node) = self.arena.get(*bus_slot) {
                    if let Some(NodeKind::Bus { items: bus_items, .. }) = node.kind() {
                        for (_key, item_slot) in bus_items {
                            // Check visibility condition if present
                            let visible = if let Some(&cond_slot) = self.visibility_conditions.get(item_slot) {
                                // Get condition value - default to visible if not evaluated
                                self.get_current_value(cond_slot)
                                    .map(|p| matches!(p, Payload::Bool(true)))
                                    .unwrap_or(true)
                            } else {
                                true
                            };

                            if visible {
                                if let Some(val) = self.get_current_value(*item_slot) {
                                    items.push(self.expand_payload_to_json_depth(val, depth + 1));
                                }
                            }
                        }
                    }
                }
                json!(items)
            }
            Payload::ObjectHandle(router_slot) => {
                let mut obj = serde_json::Map::new();
                if let Some(node) = self.arena.get(*router_slot) {
                    if let Some(NodeKind::Router { fields }) = node.kind() {
                        for (field_id, field_slot) in fields {
                            if let Some(name) = self.arena.get_field_name(*field_id) {
                                if let Some(val) = self.get_current_value(*field_slot) {
                                    obj.insert(
                                        name.to_string(),
                                        self.expand_payload_to_json_depth(val, depth + 1)
                                    );
                                }
                            }
                        }
                    }
                }
                serde_json::Value::Object(obj)
            }
            Payload::Flushed(inner) => {
                json!({"error": self.expand_payload_to_json_depth(inner, depth + 1)})
            }
            Payload::ListDelta(_) => json!("[delta]"),
            Payload::ObjectDelta(_) => json!("{delta}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_resolver_found_via_routing() {
        // Test that LinkResolver is found during traversal via routing table
        let mut el = EventLoop::new();

        // Create a simple template structure:
        // template_input (Wire) -> element (Producer) -> resolver (LinkResolver)
        //                                    ^routing
        let template_input = el.arena.alloc();
        if let Some(node) = el.arena.get_mut(template_input) {
            node.set_kind(NodeKind::Wire { source: None });
        }

        // Element node (stands in for checkbox in the real case)
        let element_slot = el.arena.alloc();
        if let Some(node) = el.arena.get_mut(element_slot) {
            node.set_kind(NodeKind::Wire { source: Some(template_input) });
        }

        // LinkResolver attached to element via routing (not via node dependencies)
        let resolver_slot = el.arena.alloc();
        let todo_elements_field = el.arena.intern_field("todo_elements");
        let todo_checkbox_field = el.arena.intern_field("todo_checkbox");
        if let Some(node) = el.arena.get_mut(resolver_slot) {
            node.set_kind(NodeKind::LinkResolver {
                input_element: element_slot,
                target_source: template_input,
                target_path: vec![todo_elements_field, todo_checkbox_field],
            });
        }

        // Add routing from element to resolver (simulates what LINK setter does)
        el.routing.add_route(element_slot, resolver_slot, Port::Input(0));

        // Create source item (the todo data object)
        let todo_checkbox_iopad = el.arena.alloc();
        if let Some(node) = el.arena.get_mut(todo_checkbox_iopad) {
            node.set_kind(NodeKind::IOPad {
                element_slot: None,
                event_type: "change".to_string(),
                connected: false,
            });
        }

        let todo_elements_router = el.arena.alloc();
        if let Some(node) = el.arena.get_mut(todo_elements_router) {
            let mut fields = std::collections::HashMap::new();
            fields.insert(todo_checkbox_field, todo_checkbox_iopad);
            node.set_kind(NodeKind::Router { fields });
        }

        let source_item = el.arena.alloc();
        if let Some(node) = el.arena.get_mut(source_item) {
            let mut fields = std::collections::HashMap::new();
            fields.insert(todo_elements_field, todo_elements_router);
            node.set_kind(NodeKind::Router { fields });
        }

        // Clone the template
        let cloned_output = el.clone_transform_subgraph_runtime(
            template_input,
            element_slot,  // template_output is the element
            source_item,
        );

        // Verify the IOPad was converted to a Wire pointing to the cloned element
        // This happens in LinkResolver processing during cloning
        if let Some(node) = el.arena.get(todo_checkbox_iopad) {
            match node.kind() {
                Some(NodeKind::Wire { source: Some(src) }) => {
                    // Success! The IOPad was converted to Wire
                    // src should point to cloned element, not original
                    assert_ne!(*src, element_slot, "Should point to cloned element, not original");
                }
                kind => panic!("Expected IOPad to be converted to Wire, got {:?}", kind),
            }
        }
    }

    #[test]
    fn link_resolver_with_intermediate_wire() {
        // Test LinkResolver when target_source is a Wire pointing to template_input
        // (not template_input directly) - this matches the real todo_mvc structure
        let mut el = EventLoop::new();

        let todo_elements_field = el.arena.intern_field("todo_elements");
        let todo_checkbox_field = el.arena.intern_field("todo_checkbox");

        // template_input is the List/map's item placeholder
        let template_input = el.arena.alloc();
        if let Some(node) = el.arena.get_mut(template_input) {
            node.set_kind(NodeKind::Wire { source: None });
        }

        // 'todo' parameter Wire in todo_item function - points to template_input
        let todo_param_wire = el.arena.alloc();
        if let Some(node) = el.arena.get_mut(todo_param_wire) {
            node.set_kind(NodeKind::Wire { source: Some(template_input) });
        }

        // Checkbox element
        let checkbox_slot = el.arena.alloc();
        if let Some(node) = el.arena.get_mut(checkbox_slot) {
            node.set_kind(NodeKind::Producer { value: Some(Payload::Text("checkbox".into())) });
        }

        // LinkResolver - target_source is todo_param_wire (NOT template_input directly)
        let resolver_slot = el.arena.alloc();
        if let Some(node) = el.arena.get_mut(resolver_slot) {
            node.set_kind(NodeKind::LinkResolver {
                input_element: checkbox_slot,
                target_source: todo_param_wire,  // Key difference: points to Wire, not template_input
                target_path: vec![todo_elements_field, todo_checkbox_field],
            });
        }

        // Routing: checkbox -> resolver
        el.routing.add_route(checkbox_slot, resolver_slot, Port::Input(0));

        // Container that holds checkbox (simulates Element/row structure)
        let children_bus = el.arena.alloc();
        if let Some(node) = el.arena.get_mut(children_bus) {
            node.set_kind(NodeKind::Bus {
                items: vec![(0, checkbox_slot)],
                alloc_site: crate::engine_v2::node::AllocSite::new(crate::engine_v2::address::SourceId::default()),
                static_item_count: 1,
            });
        }

        // Template output (Element/row result) - a TaggedObject containing the bus
        let children_field = el.arena.intern_field("children");
        let wrapper_router = el.arena.alloc();
        if let Some(node) = el.arena.get_mut(wrapper_router) {
            let mut fields = std::collections::HashMap::new();
            fields.insert(children_field, children_bus);
            node.set_kind(NodeKind::Router { fields });
        }

        let element_row_tag = el.arena.intern_tag("ElementRow");
        let template_output = el.arena.alloc();
        if let Some(node) = el.arena.get_mut(template_output) {
            node.set_kind(NodeKind::Producer {
                value: Some(Payload::TaggedObject {
                    tag: element_row_tag,
                    fields: wrapper_router,
                }),
            });
        }

        // Source item (todo data object)
        let todo_checkbox_iopad = el.arena.alloc();
        if let Some(node) = el.arena.get_mut(todo_checkbox_iopad) {
            node.set_kind(NodeKind::IOPad {
                element_slot: None,
                event_type: "change".to_string(),
                connected: false,
            });
        }

        let todo_elements_router = el.arena.alloc();
        if let Some(node) = el.arena.get_mut(todo_elements_router) {
            let mut fields = std::collections::HashMap::new();
            fields.insert(todo_checkbox_field, todo_checkbox_iopad);
            node.set_kind(NodeKind::Router { fields });
        }

        let source_item = el.arena.alloc();
        if let Some(node) = el.arena.get_mut(source_item) {
            let mut fields = std::collections::HashMap::new();
            fields.insert(todo_elements_field, todo_elements_router);
            node.set_kind(NodeKind::Router { fields });
        }

        // Debug: print what we're about to clone
        println!("template_input={:?} template_output={:?} source_item={:?}",
            template_input, template_output, source_item);
        println!("todo_param_wire={:?} checkbox_slot={:?} resolver_slot={:?}",
            todo_param_wire, checkbox_slot, resolver_slot);

        // Clone the template
        let cloned_output = el.clone_transform_subgraph_runtime(
            template_input,
            template_output,
            source_item,
        );

        println!("cloned_output={:?}", cloned_output);

        // Verify the IOPad was converted to a Wire
        if let Some(node) = el.arena.get(todo_checkbox_iopad) {
            match node.kind() {
                Some(NodeKind::Wire { source }) => {
                    println!("SUCCESS: IOPad converted to Wire with source={:?}", source);
                    assert!(source.is_some(), "Wire should have a source");
                }
                kind => panic!("Expected IOPad to be converted to Wire, got {:?}", kind),
            }
        }
    }

    #[test]
    fn event_loop_basic() {
        let mut el = EventLoop::new();
        assert_eq!(el.current_tick, 0);

        el.run_tick();
        assert_eq!(el.current_tick, 1);

        el.run_tick();
        assert_eq!(el.current_tick, 2);
    }

    #[test]
    fn event_loop_dirty_nodes() {
        let mut el = EventLoop::new();
        let slot = el.arena.alloc();

        el.mark_dirty(slot, Port::Output);
        assert_eq!(el.dirty_nodes.len(), 1);

        el.run_tick();
        assert_eq!(el.dirty_nodes.len(), 0);
    }

    #[test]
    fn message_delivery() {
        let mut el = EventLoop::new();
        let source = el.arena.alloc();
        let target = el.arena.alloc();

        // Route from source's output to target's input port 0
        el.routing.add_route(source, target, Port::Input(0));
        el.deliver_message(source, Payload::Number(42.0));

        assert_eq!(el.dirty_nodes.len(), 1);
        assert_eq!(el.dirty_nodes[0].slot, target);
        assert_eq!(el.dirty_nodes[0].port, Port::Input(0));
    }
}
