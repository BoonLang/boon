use std::collections::{BinaryHeap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use super::arena::{Arena, SlotId};
use super::address::Port;
use super::routing::RoutingTable;
use super::message::Payload;
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
    pub fn mark_dirty(&mut self, slot: SlotId, port: Port) {
        self.dirty_nodes.push(DirtyEntry { slot, port });
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
                        if let Some(NodeKind::Bus { items, alloc_site }) = bus_node.kind_mut() {
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

            NodeKind::FilteredView { source_bus, .. } => {
                // FilteredView is a compile-time construct for List/retain.
                // It just forwards the ListHandle from the source bus.
                Some(Payload::ListHandle(*source_bus))
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
                        if let Some(NodeKind::Bus { items, alloc_site }) = bus_node.kind_mut() {
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
            NodeKind::Skip { source, count, skipped } => {
                // Skip node - skips first N values, then passes through
                // Get the incoming value from source
                let source_value = self.get_current_value(*source).cloned();

                if let Some(value) = source_value {
                    if *skipped < *count {
                        // Still skipping - increment counter, don't emit
                        let new_skipped = *skipped + 1;
                        if let Some(node) = self.arena.get_mut(entry.slot) {
                            if let Some(NodeKind::Skip { skipped, .. }) = node.kind_mut() {
                                *skipped = new_skipped;
                            }
                        }
                        None
                    } else {
                        // Done skipping - pass through
                        Some(value)
                    }
                } else {
                    None
                }
            }
            NodeKind::Accumulator { sum } => {
                // Accumulator sums incoming numbers
                if let Some(Payload::Number(n)) = msg {
                    let new_sum = sum + n;
                    if let Some(node) = self.arena.get_mut(entry.slot) {
                        if let Some(NodeKind::Accumulator { sum }) = node.kind_mut() {
                            *sum = new_sum;
                        }
                    }
                    Some(Payload::Number(new_sum))
                } else {
                    Some(Payload::Number(*sum))
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

                // Check if this message is from the subscribed field (Input(1)) vs source (Input(0))
                if entry.port == Port::Input(1) && subscribed_field.is_some() {
                    // Message from subscribed field - read current value from it
                    #[cfg(target_arch = "wasm32")]
                    zoon::println!("  Extractor: reading current value from subscribed field {:?}", subscribed_field);
                    subscribed_field.and_then(|fs| self.get_current_value(fs).cloned())
                } else {
                    // Extract a field from incoming Router/TaggedObject payload
                    // or read from source's current value
                    let source_payload = msg.clone().or_else(|| {
                        source.and_then(|s| self.get_current_value(s).cloned())
                    });

                    // Helper to resolve field slot from a Router
                    let resolve_field = |arena: &Arena, router_slot: SlotId, field_id: u32| -> Option<SlotId> {
                        arena.get(router_slot)
                            .and_then(|n| n.kind())
                            .and_then(|k| match k {
                                NodeKind::Router { fields } => fields.get(&field_id).copied(),
                                _ => None,
                            })
                    };

                    // Find the field slot so we can subscribe to it
                    let field_slot = source_payload.as_ref().and_then(|payload| {
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

                    // Subscribe to the field slot for future reactive updates
                    // But only subscribe to Producer or IOPad nodes (actual data sources).
                    // Subscribing to Extractor, Arithmetic, or other computed nodes
                    // can create cycles (E1 -> A -> E2 -> E1) that cause infinite loops.
                    // IOPad nodes handle external events (button presses, input changes) and
                    // must be subscribed to for event propagation to work.
                    if let Some(fs) = field_slot {
                        // Subscribe to Producer nodes (constants/values), IOPad nodes (events),
                        // and Combiner nodes (LATEST - reactive stream mergers)
                        let is_subscribable = self.arena.get(fs)
                            .and_then(|n| n.kind())
                            .map(|k| matches!(k, NodeKind::Producer { .. } | NodeKind::IOPad { .. } | NodeKind::Combiner { .. }))
                            .unwrap_or(false);

                        if is_subscribable && *subscribed_field != Some(fs) && fs != entry.slot {
                            #[cfg(target_arch = "wasm32")]
                            zoon::println!("  Extractor: subscribing to Producer/IOPad/Combiner field slot {:?}", fs);

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
                // Otherwise, get the source value - should be an ObjectHandle or TaggedObject
                if let Some(source_value) = self.get_current_value_depth(*source_slot, depth + 1) {
                    // Get the router slot from either ObjectHandle or TaggedObject
                    let router_slot = match source_value {
                        Payload::ObjectHandle(router_slot) => Some(*router_slot),
                        Payload::TaggedObject { fields: fields_slot, .. } => Some(*fields_slot),
                        _ => None,
                    };

                    if let Some(router_slot) = router_slot {
                        // Get field from Router
                        if let Some(router_node) = self.arena.get(router_slot) {
                            if let Some(NodeKind::Router { fields }) = router_node.kind() {
                                if let Some(field_slot) = fields.get(field) {
                                    return self.get_current_value_depth(*field_slot, depth + 1);
                                }
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
                    _ => {}
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
        let mut slot_map = std::collections::HashMap::new();
        slot_map.insert(template_input, source_item); // Rewire template_input to source_item

        #[cfg(target_arch = "wasm32")]
        zoon::println!("clone_transform_subgraph_runtime: template_input={} in to_clone={}",
            template_input.index, to_clone.contains(&template_input));

        for &old_slot in &to_clone {
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

                if let Some(new_node) = self.arena.get_mut(new_slot) {
                    if let Some(kind) = new_kind {
                        new_node.set_kind(kind);
                    }
                    // Copy current value with remapped SlotIds
                    if let Some(cv) = current_value {
                        let remapped_cv = Self::remap_payload(&cv, &slot_map);
                        new_node.extension_mut().current_value = Some(remapped_cv);
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
                    Some(NodeKind::Extractor { source: Some(src), .. }) => {
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
            Some(NodeKind::Bus { items, alloc_site }) => {
                let new_items = items.iter()
                    .map(|(key, slot)| (*key, remap(slot)))
                    .collect();
                Some(NodeKind::Bus {
                    items: new_items,
                    alloc_site: alloc_site.clone(),
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
            NodeKind::FilteredView { source_bus, conditions: view_conditions } => {
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
            // No visibility condition, always visible
            true
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
            // Lists and objects not yet supported for restoration
            SerializedPayload::List(_) => return None,
            SerializedPayload::Object(_) => return None,
            SerializedPayload::TaggedObject { tag, .. } => Payload::Tag(*tag),
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
