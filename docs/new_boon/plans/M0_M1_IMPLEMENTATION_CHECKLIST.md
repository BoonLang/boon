# M0 + M1 Implementation Checklist

**Purpose:** Agent-executable checklist for implementing M0 (CLI) and M1 (New Engine in Playground).

**Progress Legend:**
- `[ ]` Not started
- `[~]` In progress
- `[x]` Complete

**Time estimate:** ~130 atomic tasks, each 5-30 minutes.

---

## PART 0: Environment Setup & Baseline

**Goal:** Verify environment is ready and establish baseline.

### 0.1 Toolchain Verification

- [ ] **0.1.1** Verify Rust toolchain
  ```bash
  rustc --version && cargo --version
  ```
  - Expected: Rust 1.75+ (stable)

- [ ] **0.1.2** Verify WASM target installed
  ```bash
  rustup target list --installed | grep wasm32
  ```
  - Expected: `wasm32-unknown-unknown`

- [ ] **0.1.3** Verify workspace compiles
  ```bash
  cargo check -p boon
  ```
  - Expected: No errors

### 0.2 Baseline Tests

- [ ] **0.2.1** Run existing test suite
  ```bash
  cargo test -p boon
  ```
  - Expected: All tests pass
  - **Record baseline:** Note number of passing tests

- [ ] **0.2.2** Verify playground can start (if available)
  ```bash
  cd playground && makers mzoon start &
  sleep 120
  curl -s http://localhost:8081 | head -5
  ```
  - Expected: HTML response (or skip if not needed for pure engine work)

### 0.3 Documentation Review

- [ ] **0.3.1** Confirm familiarity with key design docs:
  - `docs/new_boon/2.1_NODE_IDENTIFICATION.md` - SourceId, ScopeId, NodeAddress
  - `docs/new_boon/2.2_ARENA_MEMORY.md` - Arena, SlotId
  - `docs/new_boon/2.3_MESSAGE_PASSING.md` - Message, Payload, Node kinds
  - `docs/new_boon/2.4_EVENT_LOOP.md` - EventLoop, tick processing
  - `docs/new_boon/6.1_ISSUES.md` - Known issues to fix

---

## PART 1: Phase 1 - Core Types & Arena

**Prerequisites:** Part 0 complete
**Blocks:** Parts 2-12
**Goal:** Create `engine_v2/` module with foundational types.

### 1.1 Module Structure

- [ ] **1.1.1** Create `engine_v2/mod.rs`
  - File: `crates/boon/src/engine_v2/mod.rs`
  - Content:
    ```rust
    //! New arena-based reactive engine (v2)
    //!
    //! Design docs: docs/new_boon/2.x

    pub mod address;
    pub mod arena;
    pub mod node;
    pub mod message;
    pub mod event_loop;
    pub mod routing;
    // pub mod snapshot;  // Phase 8
    ```
  - Verify: File exists

- [ ] **1.1.2** Add engine_v2 to lib.rs
  - File: `crates/boon/src/lib.rs`
  - Edit: Add `pub mod engine_v2;`
  - Verify: `cargo check -p boon`

### 1.2 Address Types (§2.1)

- [ ] **1.2.1** Create `engine_v2/address.rs` with SourceId
  - File: `crates/boon/src/engine_v2/address.rs`
  - Content:
    ```rust
    use serde::{Deserialize, Serialize};

    /// Parse-time stable identifier for AST nodes.
    /// Survives whitespace/comment changes via structural hash.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub struct SourceId {
        pub stable_id: u64,    // Structural hash
        pub parse_order: u32,  // For debugging and collision tiebreaking
    }

    impl Default for SourceId {
        fn default() -> Self {
            Self { stable_id: 0, parse_order: 0 }
        }
    }
    ```
  - Verify: `cargo check -p boon`

- [ ] **1.2.2** Add ScopeId to address.rs
  - Content:
    ```rust
    /// Runtime scope identifier - captures dynamic instantiation context.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub struct ScopeId(pub u64);

    impl ScopeId {
        pub const ROOT: Self = Self(0);

        pub fn child(&self, discriminator: u64) -> Self {
            Self(self.0.wrapping_mul(31).wrapping_add(discriminator))
        }
    }

    impl Default for ScopeId {
        fn default() -> Self {
            Self::ROOT
        }
    }
    ```
  - Verify: `cargo check -p boon`

- [ ] **1.2.3** Add Domain enum to address.rs
  - Content:
    ```rust
    /// Execution domain - where a node runs.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
    pub enum Domain {
        #[default]
        Main,           // UI thread (browser main, or single-threaded mode)
        Worker(u8),     // WebWorker index
        Server,         // Backend (future: over WebSocket)
    }
    ```
  - Verify: `cargo check -p boon`

- [ ] **1.2.4** Add Port enum to address.rs
  - Content:
    ```rust
    /// Port identifier for multi-input/output nodes.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub enum Port {
        Output,           // Default output
        Input(u8),        // Numbered input (for LATEST, etc.)
        Field(u32),       // Field ID (for Router/Object)
    }

    impl Default for Port {
        fn default() -> Self {
            Self::Output
        }
    }
    ```
  - Verify: `cargo check -p boon`

- [ ] **1.2.5** Add NodeAddress struct to address.rs
  - Content:
    ```rust
    /// Full address of a reactive node port.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
    pub struct NodeAddress {
        pub domain: Domain,
        pub source_id: SourceId,
        pub scope_id: ScopeId,
        pub port: Port,
    }

    impl NodeAddress {
        pub fn new(source_id: SourceId, scope_id: ScopeId) -> Self {
            Self {
                domain: Domain::default(),
                source_id,
                scope_id,
                port: Port::Output,
            }
        }

        pub fn with_port(mut self, port: Port) -> Self {
            self.port = port;
            self
        }
    }
    ```
  - Verify: `cargo check -p boon`

- [ ] **1.2.6** Add unit tests for address types
  - Content (append to address.rs):
    ```rust
    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn scope_id_child_chain() {
            let root = ScopeId::ROOT;
            let child1 = root.child(1);
            let child2 = root.child(2);
            let grandchild = child1.child(1);

            assert_ne!(root, child1);
            assert_ne!(child1, child2);
            assert_ne!(child1, grandchild);
        }

        #[test]
        fn node_address_equality() {
            let addr1 = NodeAddress::new(
                SourceId { stable_id: 42, parse_order: 1 },
                ScopeId(100),
            );
            let addr2 = addr1.with_port(Port::Input(0));

            assert_ne!(addr1, addr2);
        }
    }
    ```
  - Verify: `cargo test -p boon engine_v2::address`

### 1.3 SlotId & Arena (§2.2)

- [ ] **1.3.1** Create `engine_v2/arena.rs` with SlotId
  - File: `crates/boon/src/engine_v2/arena.rs`
  - Content:
    ```rust
    use std::collections::HashMap;
    use super::address::NodeAddress;

    /// Generational index into the arena.
    /// Allows safe reuse of slots with use-after-free detection.
    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
    pub struct SlotId {
        pub index: u32,
        pub generation: u32,
    }

    impl SlotId {
        pub const INVALID: Self = Self { index: u32::MAX, generation: 0 };

        pub fn is_valid(&self) -> bool {
            self.index != u32::MAX
        }
    }
    ```
  - Verify: `cargo check -p boon`

- [ ] **1.3.2** Add ReactiveNode stub to arena.rs
  - Content:
    ```rust
    use super::node::NodeKind;

    /// A single reactive node in the arena.
    /// 64-byte cache-line aligned for performance.
    #[repr(C, align(64))]
    pub struct ReactiveNode {
        pub generation: u32,
        pub version: u32,
        pub dirty: bool,
        pub kind: NodeKind,
        // Connections stored in NodeKind variants
    }

    impl Default for ReactiveNode {
        fn default() -> Self {
            Self {
                generation: 0,
                version: 0,
                dirty: false,
                kind: NodeKind::Producer { value: None },
            }
        }
    }
    ```
  - Verify: `cargo check -p boon` (will fail until node.rs exists)

- [ ] **1.3.3** Create `engine_v2/node.rs` with NodeKind stub
  - File: `crates/boon/src/engine_v2/node.rs`
  - Content:
    ```rust
    use super::arena::SlotId;
    use super::message::Payload;

    /// The kind of reactive node and its kind-specific state.
    #[derive(Debug, Clone)]
    pub enum NodeKind {
        /// Constant value producer (tied signal)
        Producer { value: Option<Payload> },
        /// Named wire (variable forwarding)
        Wire { source: Option<SlotId> },
        // More kinds added in later phases
    }
    ```
  - Verify: `cargo check -p boon` (will fail until message.rs exists)

- [ ] **1.3.4** Create `engine_v2/message.rs` with Payload stub
  - File: `crates/boon/src/engine_v2/message.rs`
  - Content:
    ```rust
    use std::sync::Arc;
    use super::arena::SlotId;

    /// Payload carried by reactive messages.
    #[derive(Debug, Clone)]
    pub enum Payload {
        Number(f64),
        Text(Arc<str>),
        Tag(u32),
        Bool(bool),
        Unit,
        ListHandle(SlotId),
        ObjectHandle(SlotId),
        TaggedObject { tag: u32, fields: SlotId },
        Flushed(Box<Payload>),
    }
    ```
  - Verify: `cargo check -p boon`

- [ ] **1.3.5** Add Arena struct to arena.rs
  - Content:
    ```rust
    /// Arena allocator for reactive nodes.
    pub struct Arena {
        nodes: Vec<ReactiveNode>,
        free_list: Vec<u32>,
        /// Side table: SlotId → NodeAddress (for deterministic sorting)
        addresses: HashMap<SlotId, NodeAddress>,
    }

    impl Arena {
        pub fn new() -> Self {
            Self::with_capacity(1024)
        }

        pub fn with_capacity(capacity: usize) -> Self {
            Self {
                nodes: Vec::with_capacity(capacity),
                free_list: Vec::new(),
                addresses: HashMap::new(),
            }
        }
    }

    impl Default for Arena {
        fn default() -> Self {
            Self::new()
        }
    }
    ```
  - Verify: `cargo check -p boon`

- [ ] **1.3.6** Implement Arena::alloc
  - Content:
    ```rust
    impl Arena {
        /// Allocate a new slot in the arena.
        pub fn alloc(&mut self) -> SlotId {
            if let Some(index) = self.free_list.pop() {
                // Reuse freed slot, bump generation
                self.nodes[index as usize].generation += 1;
                SlotId {
                    index,
                    generation: self.nodes[index as usize].generation,
                }
            } else {
                // Allocate new slot
                let index = self.nodes.len() as u32;
                self.nodes.push(ReactiveNode::default());
                SlotId { index, generation: 0 }
            }
        }

        /// Allocate with associated address (for sorting).
        pub fn alloc_with_address(&mut self, addr: NodeAddress) -> SlotId {
            let slot = self.alloc();
            self.addresses.insert(slot, addr);
            slot
        }
    }
    ```
  - Verify: `cargo check -p boon`

- [ ] **1.3.7** Implement Arena::free
  - Content:
    ```rust
    impl Arena {
        /// Free a slot, making it available for reuse.
        pub fn free(&mut self, slot: SlotId) {
            if self.is_valid(slot) {
                self.free_list.push(slot.index);
                self.addresses.remove(&slot);
            }
        }

        /// Check if a SlotId is valid (correct generation).
        pub fn is_valid(&self, slot: SlotId) -> bool {
            slot.index < self.nodes.len() as u32
                && self.nodes[slot.index as usize].generation == slot.generation
        }
    }
    ```
  - Verify: `cargo check -p boon`

- [ ] **1.3.8** Implement Arena::get and get_mut
  - Content:
    ```rust
    impl Arena {
        /// Get immutable reference to node.
        pub fn get(&self, slot: SlotId) -> Option<&ReactiveNode> {
            if self.is_valid(slot) {
                Some(&self.nodes[slot.index as usize])
            } else {
                None
            }
        }

        /// Get mutable reference to node.
        pub fn get_mut(&mut self, slot: SlotId) -> Option<&mut ReactiveNode> {
            if self.is_valid(slot) {
                Some(&mut self.nodes[slot.index as usize])
            } else {
                None
            }
        }

        /// Get address for a slot (for sorting).
        pub fn get_address(&self, slot: SlotId) -> Option<&NodeAddress> {
            self.addresses.get(&slot)
        }
    }
    ```
  - Verify: `cargo check -p boon`

- [ ] **1.3.9** Add Arena unit tests
  - Content (append to arena.rs):
    ```rust
    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn arena_alloc_and_free() {
            let mut arena = Arena::new();

            let slot1 = arena.alloc();
            let slot2 = arena.alloc();

            assert!(arena.is_valid(slot1));
            assert!(arena.is_valid(slot2));
            assert_ne!(slot1, slot2);

            arena.free(slot1);
            assert!(!arena.is_valid(slot1));

            // Reuse freed slot
            let slot3 = arena.alloc();
            assert_eq!(slot3.index, slot1.index);
            assert_ne!(slot3.generation, slot1.generation);
        }

        #[test]
        fn arena_get_mut() {
            let mut arena = Arena::new();
            let slot = arena.alloc();

            {
                let node = arena.get_mut(slot).unwrap();
                node.dirty = true;
            }

            let node = arena.get(slot).unwrap();
            assert!(node.dirty);
        }

        #[test]
        fn arena_generation_check() {
            let mut arena = Arena::new();

            let slot1 = arena.alloc();
            arena.free(slot1);
            let slot2 = arena.alloc(); // Reuses slot1's index

            // Old slot ID should be invalid
            assert!(!arena.is_valid(slot1));
            assert!(arena.is_valid(slot2));
        }
    }
    ```
  - Verify: `cargo test -p boon engine_v2::arena`

### 1.4 EventLoop Skeleton

- [ ] **1.4.1** Create `engine_v2/event_loop.rs` skeleton
  - File: `crates/boon/src/engine_v2/event_loop.rs`
  - Content:
    ```rust
    use std::collections::{BinaryHeap, VecDeque};
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use super::arena::{Arena, SlotId};
    use super::address::Port;

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
            }
        }

        /// Mark a node as dirty (needs reprocessing).
        pub fn mark_dirty(&mut self, slot: SlotId, port: Port) {
            self.dirty_nodes.push(DirtyEntry { slot, port });
        }
    }

    impl Default for EventLoop {
        fn default() -> Self {
            Self::new()
        }
    }
    ```
  - Verify: `cargo check -p boon`

- [ ] **1.4.2** Add EventLoop::run_tick skeleton
  - Content:
    ```rust
    impl EventLoop {
        /// Run one tick of the event loop.
        /// Processes all dirty nodes until quiescence.
        pub fn run_tick(&mut self) {
            self.in_tick.store(true, Ordering::SeqCst);
            self.tick_scheduled.store(false, Ordering::SeqCst);
            self.current_tick += 1;

            // Phase 1: Process timers (Phase 6 will implement)
            // self.process_timers();

            // Phase 2: Process until quiescence
            while !self.dirty_nodes.is_empty() {
                let to_process: Vec<_> = self.dirty_nodes.drain(..).collect();
                for entry in to_process {
                    self.process_node(entry);
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
            // Placeholder - implemented in Phase 3
            let _ = entry;
        }
    }
    ```
  - Verify: `cargo check -p boon`

- [ ] **1.4.3** Add EventLoop unit tests
  - Content:
    ```rust
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
    }
    ```
  - Verify: `cargo test -p boon engine_v2::event_loop`

### 1.5 Phase 1 Verification

- [ ] **1.5.1** Run all engine_v2 tests
  ```bash
  cargo test -p boon engine_v2
  ```
  - Expected: All tests pass

- [ ] **1.5.2** Verify no regressions
  ```bash
  cargo test -p boon
  ```
  - Expected: All tests pass (same as baseline)

- [ ] **1.5.3** Commit Phase 1
  ```bash
  git add -A && git commit -m "Phase 1: Core types and arena for engine_v2"
  ```

**If stuck:**
- Check module exports in mod.rs
- Verify all `use` statements are correct
- Run `cargo check` after each small change

---

## PART 2: Phase 2 - Message & Routing

**Prerequisites:** Part 1 complete
**Blocks:** Parts 3-12
**Goal:** Implement message passing infrastructure.

### 2.1 Complete Payload Enum

- [ ] **2.1.1** Add ListDelta to message.rs
  - Content:
    ```rust
    /// Key identifying a list item (from AllocSite).
    pub type ItemKey = u64;

    /// Delta for efficient list updates.
    #[derive(Clone, Debug)]
    pub enum ListDelta {
        Insert { key: ItemKey, index: u32, initial: Payload },
        Update { key: ItemKey, value: Payload },
        Remove { key: ItemKey },
        Move { key: ItemKey, from_index: u32, to_index: u32 },
        Replace { items: Vec<(ItemKey, Payload)> },
    }
    ```
  - Verify: `cargo check -p boon`

- [ ] **2.1.2** Add ObjectDelta to message.rs
  - Content:
    ```rust
    /// Field identifier (interned string ID).
    pub type FieldId = u32;

    /// Delta for efficient object updates.
    #[derive(Clone, Debug)]
    pub enum ObjectDelta {
        FieldUpdate { field: FieldId, value: Payload },
        FieldRemove { field: FieldId },
    }
    ```
  - Verify: `cargo check -p boon`

- [ ] **2.1.3** Update Payload enum with deltas
  - Content: Add to Payload enum:
    ```rust
    pub enum Payload {
        // ... existing variants ...
        ListDelta(ListDelta),
        ObjectDelta(ObjectDelta),
    }
    ```
  - Verify: `cargo check -p boon`

### 2.2 Message Struct

- [ ] **2.2.1** Add Message struct to message.rs
  - Content:
    ```rust
    use super::address::NodeAddress;

    /// A message sent between reactive nodes.
    #[derive(Clone, Debug)]
    pub struct Message {
        pub source: NodeAddress,
        pub payload: Payload,
        pub version: u64,
        pub idempotency_key: u64,
    }

    impl Message {
        pub fn new(source: NodeAddress, payload: Payload) -> Self {
            Self {
                source,
                payload,
                version: 0,
                idempotency_key: 0,
            }
        }
    }
    ```
  - Verify: `cargo check -p boon`

### 2.3 Routing Table

- [ ] **2.3.1** Create `engine_v2/routing.rs`
  - File: `crates/boon/src/engine_v2/routing.rs`
  - Content:
    ```rust
    use std::collections::HashMap;
    use super::arena::SlotId;
    use super::address::Port;

    /// Routes messages between nodes.
    #[derive(Debug, Default)]
    pub struct RoutingTable {
        /// source_slot -> [(target_slot, target_port)]
        routes: HashMap<SlotId, Vec<(SlotId, Port)>>,
    }

    impl RoutingTable {
        pub fn new() -> Self {
            Self::default()
        }

        /// Add a route from source to target.
        pub fn add_route(&mut self, source: SlotId, target: SlotId, port: Port) {
            self.routes
                .entry(source)
                .or_default()
                .push((target, port));
        }

        /// Remove a route from source to target.
        pub fn remove_route(&mut self, source: SlotId, target: SlotId, port: Port) {
            if let Some(targets) = self.routes.get_mut(&source) {
                targets.retain(|(t, p)| !(*t == target && *p == port));
            }
        }

        /// Get all targets subscribed to a source.
        pub fn get_subscribers(&self, source: SlotId) -> &[(SlotId, Port)] {
            self.routes.get(&source).map(|v| v.as_slice()).unwrap_or(&[])
        }

        /// Remove all routes involving a slot (when freed).
        pub fn remove_slot(&mut self, slot: SlotId) {
            self.routes.remove(&slot);
            for targets in self.routes.values_mut() {
                targets.retain(|(t, _)| *t != slot);
            }
        }
    }
    ```
  - Verify: `cargo check -p boon`

- [ ] **2.3.2** Add routing tests
  - Content:
    ```rust
    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn routing_add_remove() {
            let mut rt = RoutingTable::new();
            let s1 = SlotId { index: 1, generation: 0 };
            let s2 = SlotId { index: 2, generation: 0 };
            let s3 = SlotId { index: 3, generation: 0 };

            rt.add_route(s1, s2, Port::Output);
            rt.add_route(s1, s3, Port::Input(0));

            let subs = rt.get_subscribers(s1);
            assert_eq!(subs.len(), 2);

            rt.remove_route(s1, s2, Port::Output);
            let subs = rt.get_subscribers(s1);
            assert_eq!(subs.len(), 1);
        }

        #[test]
        fn routing_remove_slot() {
            let mut rt = RoutingTable::new();
            let s1 = SlotId { index: 1, generation: 0 };
            let s2 = SlotId { index: 2, generation: 0 };

            rt.add_route(s1, s2, Port::Output);
            rt.remove_slot(s1);

            assert!(rt.get_subscribers(s1).is_empty());
        }
    }
    ```
  - Verify: `cargo test -p boon engine_v2::routing`

### 2.4 Integrate Routing with EventLoop

- [ ] **2.4.1** Add RoutingTable to EventLoop
  - File: `crates/boon/src/engine_v2/event_loop.rs`
  - Edit: Add field and update imports:
    ```rust
    use super::routing::RoutingTable;

    pub struct EventLoop {
        // ... existing fields ...
        pub routing: RoutingTable,
    }

    impl EventLoop {
        pub fn new() -> Self {
            Self {
                // ... existing fields ...
                routing: RoutingTable::new(),
            }
        }
    }
    ```
  - Verify: `cargo check -p boon`

- [ ] **2.4.2** Add deliver_message method
  - Content:
    ```rust
    use super::message::{Message, Payload};

    impl EventLoop {
        /// Deliver a message to all subscribers of a source node.
        pub fn deliver_message(&mut self, source: SlotId, payload: Payload) {
            let subscribers: Vec<_> = self.routing
                .get_subscribers(source)
                .to_vec();

            for (target, port) in subscribers {
                self.mark_dirty(target, port);
                // In full implementation, would also store the message
            }
        }
    }
    ```
  - Verify: `cargo check -p boon`

### 2.5 Phase 2 Verification

- [ ] **2.5.1** Add integration test for message flow
  - Content (in event_loop.rs tests):
    ```rust
    #[test]
    fn message_delivery() {
        let mut el = EventLoop::new();
        let source = el.arena.alloc();
        let target = el.arena.alloc();

        el.routing.add_route(source, target, Port::Output);
        el.deliver_message(source, Payload::Number(42.0));

        assert_eq!(el.dirty_nodes.len(), 1);
        assert_eq!(el.dirty_nodes[0].slot, target);
    }
    ```
  - Verify: `cargo test -p boon engine_v2::event_loop`

- [ ] **2.5.2** Run all tests
  ```bash
  cargo test -p boon engine_v2
  ```
  - Expected: All tests pass

- [ ] **2.5.3** Commit Phase 2
  ```bash
  git add -A && git commit -m "Phase 2: Message passing and routing table"
  ```

---

## PART 3: Phase 3 - Basic Nodes

**Prerequisites:** Part 2 complete
**Blocks:** Parts 4-12
**Goal:** Implement Producer, Wire, Router nodes and basic evaluator.

### 3.1 Expand NodeKind

- [ ] **3.1.1** Add Router variant to NodeKind
  - File: `crates/boon/src/engine_v2/node.rs`
  - Content:
    ```rust
    use std::collections::HashMap;
    use super::message::FieldId;

    #[derive(Debug, Clone)]
    pub enum NodeKind {
        /// Constant value producer (tied signal)
        Producer { value: Option<Payload> },

        /// Named wire (variable forwarding)
        Wire { source: Option<SlotId> },

        /// Object demultiplexer - routes to field slots
        Router { fields: HashMap<FieldId, SlotId> },
    }
    ```
  - Verify: `cargo check -p boon`

### 3.2 Node Processing

- [ ] **3.2.1** Implement process_node in EventLoop
  - Content:
    ```rust
    impl EventLoop {
        fn process_node(&mut self, entry: DirtyEntry) {
            let Some(node) = self.arena.get(entry.slot) else {
                return;
            };

            let output = match &node.kind {
                NodeKind::Producer { value } => value.clone(),
                NodeKind::Wire { source } => {
                    source.and_then(|s| {
                        self.arena.get(s).and_then(|n| match &n.kind {
                            NodeKind::Producer { value } => value.clone(),
                            _ => None,
                        })
                    })
                }
                NodeKind::Router { .. } => None, // Router doesn't emit directly
            };

            if let Some(payload) = output {
                self.deliver_message(entry.slot, payload);
            }
        }
    }
    ```
  - Verify: `cargo check -p boon`

### 3.3 Create Basic Evaluator

- [ ] **3.3.1** Create `evaluator_v2/mod.rs`
  - File: `crates/boon/src/evaluator_v2/mod.rs`
  - Content:
    ```rust
    //! Evaluator for the new arena-based engine.
    //!
    //! Compiles AST expressions into reactive nodes in the arena.

    use crate::engine_v2::{
        arena::SlotId,
        event_loop::EventLoop,
        message::Payload,
        node::NodeKind,
    };

    /// Context for compilation.
    pub struct CompileContext<'a> {
        pub event_loop: &'a mut EventLoop,
    }

    impl<'a> CompileContext<'a> {
        pub fn new(event_loop: &'a mut EventLoop) -> Self {
            Self { event_loop }
        }

        /// Compile a constant value.
        pub fn compile_constant(&mut self, value: Payload) -> SlotId {
            let slot = self.event_loop.arena.alloc();
            if let Some(node) = self.event_loop.arena.get_mut(slot) {
                node.kind = NodeKind::Producer { value: Some(value) };
            }
            slot
        }

        /// Compile a wire (alias to another slot).
        pub fn compile_wire(&mut self, source: SlotId) -> SlotId {
            let slot = self.event_loop.arena.alloc();
            if let Some(node) = self.event_loop.arena.get_mut(slot) {
                node.kind = NodeKind::Wire { source: Some(source) };
            }
            // Subscribe to source
            self.event_loop.routing.add_route(source, slot, crate::engine_v2::address::Port::Output);
            slot
        }
    }
    ```
  - Verify: `cargo check -p boon` (may fail if evaluator_v2 not in lib.rs)

- [ ] **3.3.2** Add evaluator_v2 to lib.rs
  - File: `crates/boon/src/lib.rs`
  - Edit: Add `pub mod evaluator_v2;`
  - Verify: `cargo check -p boon`

### 3.4 Object (Router) Support

- [ ] **3.4.1** Add compile_object to CompileContext
  - Content:
    ```rust
    use std::collections::HashMap;
    use crate::engine_v2::message::FieldId;

    impl<'a> CompileContext<'a> {
        /// Compile an object with fields.
        pub fn compile_object(&mut self, fields: Vec<(FieldId, SlotId)>) -> SlotId {
            let slot = self.event_loop.arena.alloc();
            let field_map: HashMap<FieldId, SlotId> = fields.into_iter().collect();

            if let Some(node) = self.event_loop.arena.get_mut(slot) {
                node.kind = NodeKind::Router { fields: field_map };
            }

            slot
        }

        /// Get a field from a Router node.
        pub fn get_field(&self, router: SlotId, field: FieldId) -> Option<SlotId> {
            let node = self.event_loop.arena.get(router)?;
            match &node.kind {
                NodeKind::Router { fields } => fields.get(&field).copied(),
                _ => None,
            }
        }
    }
    ```
  - Verify: `cargo check -p boon`

### 3.5 Phase 3 Tests

- [ ] **3.5.1** Add evaluator tests
  - Content (in evaluator_v2/mod.rs):
    ```rust
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
            match &node.kind {
                NodeKind::Producer { value: Some(Payload::Number(n)) } => {
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
    }
    ```
  - Verify: `cargo test -p boon evaluator_v2`

### 3.6 Phase 3 Verification

- [ ] **3.6.1** Run all engine_v2 tests
  ```bash
  cargo test -p boon engine_v2
  cargo test -p boon evaluator_v2
  ```
  - Expected: All tests pass

- [ ] **3.6.2** Commit Phase 3
  ```bash
  git add -A && git commit -m "Phase 3: Basic nodes (Producer, Wire, Router) and evaluator"
  ```

---

## PART 4: Phase 3.5 - CLI Test Harness (M0 Foundation)

**Prerequisites:** Part 3 complete
**Blocks:** Parts 5-12
**Goal:** Create minimal CLI for fast native testing.

### 4.1 CLI Platform Module

- [ ] **4.1.1** Create `platform/cli/mod.rs`
  - File: `crates/boon/src/platform/cli/mod.rs`
  - Content:
    ```rust
    //! CLI platform implementation for the new engine.

    pub mod runtime;
    pub mod storage;
    ```
  - Verify: File exists

- [ ] **4.1.2** Create `platform/cli/runtime.rs`
  - File: `crates/boon/src/platform/cli/runtime.rs`
  - Content:
    ```rust
    //! Tokio-based runtime for CLI.

    use crate::engine_v2::event_loop::EventLoop;

    /// Run the event loop until quiescent (no pending work).
    pub fn run_until_quiescent(event_loop: &mut EventLoop) {
        // Run ticks until no more dirty nodes
        loop {
            let had_dirty = !event_loop.dirty_nodes.is_empty();
            event_loop.run_tick();

            if !had_dirty && event_loop.dirty_nodes.is_empty() {
                break;
            }
        }
    }
    ```
  - Verify: `cargo check -p boon --features cli` (may need feature)

- [ ] **4.1.3** Create `platform/cli/storage.rs`
  - File: `crates/boon/src/platform/cli/storage.rs`
  - Content:
    ```rust
    //! File-based persistence for CLI.

    use std::path::PathBuf;
    use std::fs;

    pub struct FileStorage {
        base_path: PathBuf,
    }

    impl FileStorage {
        pub fn new(base_path: PathBuf) -> Self {
            Self { base_path }
        }

        pub fn load(&self, key: &str) -> Option<String> {
            let path = self.base_path.join(format!("{}.json", key));
            fs::read_to_string(path).ok()
        }

        pub fn save(&self, key: &str, value: &str) -> std::io::Result<()> {
            fs::create_dir_all(&self.base_path)?;
            let path = self.base_path.join(format!("{}.json", key));
            fs::write(path, value)
        }

        pub fn remove(&self, key: &str) -> std::io::Result<()> {
            let path = self.base_path.join(format!("{}.json", key));
            fs::remove_file(path)
        }
    }
    ```
  - Verify: `cargo check -p boon`

- [ ] **4.1.4** Add cli module to platform
  - File: `crates/boon/src/platform/mod.rs` (create if needed)
  - Content:
    ```rust
    #[cfg(feature = "cli")]
    pub mod cli;
    ```
  - File: `crates/boon/src/lib.rs`
  - Edit: Add `pub mod platform;` if not exists
  - Verify: `cargo check -p boon`

### 4.2 Value Materialization

- [ ] **4.2.1** Add materialize function to message.rs
  - Content:
    ```rust
    impl Payload {
        /// Convert payload to JSON for CLI output.
        pub fn to_json(&self) -> serde_json::Value {
            use serde_json::json;
            match self {
                Payload::Number(n) => json!(n),
                Payload::Text(s) => json!(s.as_ref()),
                Payload::Bool(b) => json!(b),
                Payload::Unit => json!(null),
                Payload::Tag(t) => json!(format!("Tag({})", t)),
                Payload::TaggedObject { tag, .. } => json!({"_tag": tag}),
                Payload::ListHandle(_) => json!("[list]"),
                Payload::ObjectHandle(_) => json!("{object}"),
                Payload::Flushed(inner) => json!({"error": inner.to_json()}),
                Payload::ListDelta(_) => json!("[delta]"),
                Payload::ObjectDelta(_) => json!("{delta}"),
            }
        }
    }
    ```
  - Verify: `cargo check -p boon`

### 4.3 Create boon-cli Crate

- [ ] **4.3.1** Create `crates/boon-cli/Cargo.toml`
  - File: `crates/boon-cli/Cargo.toml`
  - Content:
    ```toml
    [package]
    name = "boon-cli"
    version = "0.1.0"
    edition = "2021"

    [dependencies]
    boon = { path = "../boon", features = ["cli"] }
    clap = { version = "4", features = ["derive"] }
    serde_json = "1"
    tokio = { version = "1", features = ["rt", "macros"] }
    ```
  - Verify: File exists

- [ ] **4.3.2** Create `crates/boon-cli/src/main.rs`
  - File: `crates/boon-cli/src/main.rs`
  - Content:
    ```rust
    use clap::{Parser, Subcommand};
    use std::path::PathBuf;

    #[derive(Parser)]
    #[command(name = "boon")]
    #[command(about = "Boon language CLI")]
    struct Cli {
        #[command(subcommand)]
        command: Commands,
    }

    #[derive(Subcommand)]
    enum Commands {
        /// Evaluate inline Boon code
        Eval {
            /// The code to evaluate
            code: String,
        },
        /// Run a Boon file
        Run {
            /// Path to .bn file
            file: PathBuf,
        },
    }

    fn main() {
        let cli = Cli::parse();

        match cli.command {
            Commands::Eval { code } => {
                println!("Would evaluate: {}", code);
                // TODO: Parse and evaluate
            }
            Commands::Run { file } => {
                println!("Would run: {}", file.display());
                // TODO: Parse and run file
            }
        }
    }
    ```
  - Verify: `cargo check -p boon-cli`

- [ ] **4.3.3** Add boon-cli to workspace
  - File: `Cargo.toml` (workspace root)
  - Edit: Add `"crates/boon-cli"` to members
  - Verify: `cargo check -p boon-cli`

### 4.4 Add cli Feature to boon

- [ ] **4.4.1** Add cli feature to boon's Cargo.toml
  - File: `crates/boon/Cargo.toml`
  - Edit: Add under `[features]`:
    ```toml
    [features]
    default = []
    cli = ["serde_json"]
    ```
  - Verify: `cargo check -p boon --features cli`

### 4.5 Phase 3.5 Verification

- [ ] **4.5.1** Verify CLI builds
  ```bash
  cargo build -p boon-cli
  ```
  - Expected: Builds successfully

- [ ] **4.5.2** Test CLI stub
  ```bash
  cargo run -p boon-cli -- eval "1 + 2"
  ```
  - Expected: "Would evaluate: 1 + 2"

- [ ] **4.5.3** Commit Phase 3.5
  ```bash
  git add -A && git commit -m "Phase 3.5: CLI test harness foundation"
  ```

---

## PART 5: Phase 4 - Combinators

**Prerequisites:** Part 4 complete
**Goal:** Implement LATEST, HOLD, THEN, WHEN, WHILE nodes.

### 5.1 Add Combiner Node (LATEST)

- [ ] **5.1.1** Add Combiner to NodeKind
  - Content:
    ```rust
    /// Multi-input combiner (LATEST) - emits when any input changes
    Combiner {
        inputs: Vec<SlotId>,
        last_values: Vec<Option<Payload>>,
    },
    ```
  - Verify: `cargo check -p boon`

### 5.2 Add Register Node (HOLD)

- [ ] **5.2.1** Add Register to NodeKind
  - Content:
    ```rust
    /// State holder (HOLD) - D flip-flop equivalent
    Register {
        stored_value: Option<Payload>,
        body_input: Option<SlotId>,
    },
    ```
  - Verify: `cargo check -p boon`

### 5.3 Add Transformer Node (THEN)

- [ ] **5.3.1** Add Transformer to NodeKind
  - Content:
    ```rust
    /// Combinational logic (THEN) - transforms input on arrival
    Transformer {
        input: Option<SlotId>,
        // transform_fn stored separately or inlined
    },
    ```
  - Verify: `cargo check -p boon`

### 5.4 Add PatternMux Node (WHEN)

- [ ] **5.4.1** Add PatternMux to NodeKind
  - Content:
    ```rust
    /// Pattern decoder (WHEN) - matches patterns and routes
    PatternMux {
        input: Option<SlotId>,
        arms: Vec<SlotId>,  // Output per arm
    },
    ```
  - Verify: `cargo check -p boon`

### 5.5 Add SwitchedWire Node (WHILE)

- [ ] **5.5.1** Add SwitchedWire to NodeKind
  - Content:
    ```rust
    /// Tri-state buffer (WHILE) - continuous while pattern matches
    SwitchedWire {
        input: Option<SlotId>,
        current_arm: Option<usize>,
        arm_outputs: Vec<SlotId>,
    },
    ```
  - Verify: `cargo check -p boon`

### 5.6 Implement Node Processing

- [ ] **5.6.1** Expand process_node for combinators
  - Update EventLoop::process_node to handle new node kinds
  - (Implementation details depend on message storage approach)

### 5.7 Add Combinator Compilation

- [ ] **5.7.1** Add compile_hold to CompileContext
  - Content: Create HOLD node with initial value and body

- [ ] **5.7.2** Add compile_latest to CompileContext
  - Content: Create Combiner with multiple inputs

- [ ] **5.7.3** Add compile_then to CompileContext
  - Content: Create Transformer with input

### 5.8 Phase 4 Verification

- [ ] **5.8.1** Test counter pattern
  ```rust
  // Manual test: counter increments on trigger
  let initial = ctx.compile_constant(Payload::Number(0.0));
  let hold = ctx.compile_hold(initial, body_slot);
  ```

- [ ] **5.8.2** Run all tests
  ```bash
  cargo test -p boon engine_v2
  cargo test -p boon evaluator_v2
  ```

- [ ] **5.8.3** Commit Phase 4
  ```bash
  git add -A && git commit -m "Phase 4: Combinators (LATEST, HOLD, THEN, WHEN, WHILE)"
  ```

---

## PART 6: Phase 4.5 - FUNCTION + BLOCK

**Prerequisites:** Part 5 complete
**Goal:** Implement FUNCTION definitions and BLOCK local bindings.

### 6.1 FUNCTION Support

- [ ] **6.1.1** Add function registry to CompileContext
- [ ] **6.1.2** Implement function call compilation
- [ ] **6.1.3** Test function definitions

### 6.2 BLOCK Support (Issue 19)

- [ ] **6.2.1** Implement BLOCK as lexical scope with local bindings
- [ ] **6.2.2** Test BLOCK local bindings
- [ ] **6.2.3** Verify BLOCK doesn't create new scope (same ScopeId)

### 6.3 Phase 4.5 Verification

- [ ] **6.3.1** Test FUNCTION definition and call
- [ ] **6.3.2** Test BLOCK inside various contexts
- [ ] **6.3.3** Commit Phase 4.5

---

## PART 7: Phase 5 - Lists

**Prerequisites:** Part 6 complete
**Goal:** Implement Bus (List) with append/remove/map operations.

### 7.1 Add Bus Node (List)

- [ ] **7.1.1** Add Bus to NodeKind
  - Content:
    ```rust
    /// Dynamic wire collection (List) - address decoder
    Bus {
        items: Vec<(ItemKey, SlotId)>,
        alloc_site: AllocSite,
    },
    ```

### 7.2 Implement AllocSite

- [ ] **7.2.1** Add AllocSite struct
  - Content:
    ```rust
    pub struct AllocSite {
        pub site_source_id: SourceId,
        pub next_instance: u64,
    }

    impl AllocSite {
        pub fn allocate(&mut self) -> ItemKey {
            let id = self.next_instance;
            self.next_instance += 1;
            id
        }
    }
    ```

### 7.3 List Operations

- [ ] **7.3.1** Implement List/append
- [ ] **7.3.2** Implement List/remove with removed-set (Issue 17)
- [ ] **7.3.3** Implement List/map with external deps (Issue 16)
- [ ] **7.3.4** Implement List/retain, List/count, List/is_empty

### 7.4 Phase 5 Verification

- [ ] **7.4.1** Test list append/remove
- [ ] **7.4.2** Test chained List/remove (Issue 17 fix)
- [ ] **7.4.3** Test List/map external dependencies (Issue 16 fix)
- [ ] **7.4.4** Commit Phase 5

---

## PART 8: Phase 6 - Timer, Events & Effects

**Prerequisites:** Part 7 complete
**Goal:** Implement timer queue, Stream/pulses, Stream/skip, EffectNode.

### 8.1 Timer Queue

- [ ] **8.1.1** Implement process_timers in EventLoop
- [ ] **8.1.2** Implement Timer/interval API

### 8.2 Stream Operators

- [ ] **8.2.1** Implement Stream/pulses (Issue 27 - sequential)
  - One pulse per tick, not batched
- [ ] **8.2.2** Implement Stream/skip

### 8.3 Effect Node (Issue 18)

- [ ] **8.3.1** Add EffectNode to NodeKind
- [ ] **8.3.2** Implement Log/info effect
- [ ] **8.3.3** Implement effect execution at tick end

### 8.4 IOPad Node (LINK)

- [ ] **8.4.1** Add IOPad to NodeKind (skeleton)

### 8.5 Phase 6 Verification

- [ ] **8.5.1** Test Timer/interval
- [ ] **8.5.2** Test Stream/pulses
- [ ] **8.5.3** Test fibonacci.bn pattern
- [ ] **8.5.4** Commit Phase 6

---

## PART 9: Phase 7 - Bridge & Playground

**Prerequisites:** Part 8 complete
**Goal:** Connect new engine to Zoon UI.

### 9.1 Bridge Module

- [ ] **9.1.1** Create `platform/browser/bridge_v2.rs`
- [ ] **9.1.2** Implement arena → Zoon element conversion
- [ ] **9.1.3** Handle scalar roots (Text, Number, Bool, Unit) - see 2.8

### 9.2 PASS/PASSED Context

- [ ] **9.2.1** Add pass_stack to CompileContext
- [ ] **9.2.2** Implement PASS: argument handling
- [ ] **9.2.3** Implement PASSED resolution

### 9.3 TextTemplate Node (Issue 4)

- [ ] **9.3.1** Add TextTemplate to NodeKind
- [ ] **9.3.2** Implement reactive TEXT interpolation

### 9.4 Element States (Issue 7)

- [ ] **9.4.1** Add ElementState struct
- [ ] **9.4.2** Implement hovered/focused streams

### 9.5 LINK Protocol (Issue 15)

- [ ] **9.5.1** Implement LINK bind/unbind
- [ ] **9.5.2** Implement scope finalization for LINK cleanup

### 9.6 Document/new and Element/*

- [ ] **9.6.1** Implement Document/new
- [ ] **9.6.2** Implement Element/button, Element/label, etc.

### 9.7 Feature Flag

- [ ] **9.7.1** Add engine-v2 feature flag
- [ ] **9.7.2** Wire up interpreter to use new engine conditionally

### 9.8 Phase 7 Verification

- [ ] **9.8.1** Test shopping_list.bn in playground
- [ ] **9.8.2** Test switch_hold_test.bn (Issue 15)
- [ ] **9.8.3** Test button_hover_test.bn (Issue 7)
- [ ] **9.8.4** Test todo_mvc.bn (full validation)
- [ ] **9.8.5** Commit Phase 7

---

## PART 10: Phase 8 - Snapshot System

**Prerequisites:** Part 9 complete
**Goal:** Implement state persistence.

### 10.1 GraphSnapshot

- [ ] **10.1.1** Create `engine_v2/snapshot.rs`
- [ ] **10.1.2** Implement GraphSnapshot struct
- [ ] **10.1.3** Implement serialize_arena
- [ ] **10.1.4** Implement restore_arena

### 10.2 Integration

- [ ] **10.2.1** Integrate with localStorage (browser)
- [ ] **10.2.2** Integrate with file storage (CLI)

### 10.3 Phase 8 Verification

- [ ] **10.3.1** Test state persistence across page reload
- [ ] **10.3.2** Test CLI state persistence
- [ ] **10.3.3** Commit Phase 8

---

## PART 11: Phase 9 - CLI Completion (M0 COMPLETE)

**Prerequisites:** Part 10 complete
**Goal:** Complete CLI with full functionality.

### 11.1 Complete boon Commands

- [ ] **11.1.1** Implement full `boon eval` with parsing
- [ ] **11.1.2** Implement `boon run file.bn`
- [ ] **11.1.3** Implement `--ticks N` flag
- [ ] **11.1.4** Implement `--ms N` flag

### 11.2 Golden File Testing

- [ ] **11.2.1** Implement `boon test` command
- [ ] **11.2.2** Implement `boon test --update`
- [ ] **11.2.3** Create test file format parser

### 11.3 Phase 9 Verification (M0 COMPLETE)

- [ ] **11.3.1** `boon run examples/counter.bn` works
- [ ] **11.3.2** `boon run examples/interval.bn --ticks 5` works
- [ ] **11.3.3** `boon test tests/*.bn` works
- [ ] **11.3.4** Commit Phase 9

---

## PART 12: Final Validation

**Prerequisites:** Parts 1-11 complete
**Goal:** Verify M0 and M1 success criteria.

### 12.1 M0 Success Criteria

- [ ] **12.1.1** `boon run file.bn` executes and outputs JSON
- [ ] **12.1.2** `boon eval "code"` works
- [ ] **12.1.3** `boon test` runs test files
- [ ] **12.1.4** Timer/interval works in CLI
- [ ] **12.1.5** File-based persistence works

### 12.2 M1 Success Criteria

- [ ] **12.2.1** `engine_v2/` compiles without browser deps
- [ ] **12.2.2** counter.bn works with new engine
- [ ] **12.2.3** interval.bn works with new engine
- [ ] **12.2.4** todo_mvc.bn works with new engine
- [ ] **12.2.5** State persists across page reload
- [ ] **12.2.6** Feature flag switches between old/new engine
- [ ] **12.2.7** No performance regression

### 12.3 Example Coverage

Run all 23 validation examples:

- [ ] **12.3.1** minimal.bn
- [ ] **12.3.2** hello_world.bn
- [ ] **12.3.3** interval.bn
- [ ] **12.3.4** interval_hold.bn
- [ ] **12.3.5** counter.bn
- [ ] **12.3.6** counter_hold.bn
- [ ] **12.3.7** fibonacci.bn
- [ ] **12.3.8** layers.bn
- [ ] **12.3.9** shopping_list.bn
- [ ] **12.3.10** pages.bn
- [ ] **12.3.11** todo_mvc.bn
- [ ] **12.3.12** list_retain_count.bn
- [ ] **12.3.13** list_map_block.bn
- [ ] **12.3.14** list_object_state.bn
- [ ] **12.3.15** list_retain_reactive.bn
- [ ] **12.3.16** list_retain_remove.bn
- [ ] **12.3.17** while_function_call.bn
- [ ] **12.3.18** list_map_external_dep.bn (Issue 16 fix verified)
- [ ] **12.3.19** text_interpolation_update.bn (Issue 4 fix verified)
- [ ] **12.3.20** button_hover_test.bn (Issue 7 fix verified)
- [ ] **12.3.21** switch_hold_test.bn (Issue 15 fix verified)
- [ ] **12.3.22** filter_checkbox_bug.bn (Issues 15, 16 fix verified)
- [ ] **12.3.23** chained_list_remove_bug.bn (Issue 17 fix verified)

### 12.4 Final Commit

- [ ] **12.4.1** All tests pass
- [ ] **12.4.2** Update 6.5_SYNC.md with completion status
- [ ] **12.4.3** Final commit
  ```bash
  git add -A && git commit -m "M0 + M1 complete: New arena-based engine + CLI"
  ```

---

## Quick Reference

### Key Commands

```bash
# Build checks
cargo check -p boon
cargo check -p boon --features cli
cargo check -p boon-cli

# Tests
cargo test -p boon engine_v2
cargo test -p boon evaluator_v2
cargo test -p boon

# CLI
cargo run -p boon-cli -- eval "1 + 2"
cargo run -p boon-cli -- run examples/counter.bn

# Playground
cd playground && makers mzoon start &
```

### File Locations

| Module | Path |
|--------|------|
| engine_v2 | `crates/boon/src/engine_v2/` |
| evaluator_v2 | `crates/boon/src/evaluator_v2/` |
| CLI platform | `crates/boon/src/platform/cli/` |
| CLI binary | `crates/boon-cli/src/main.rs` |
| Bridge | `crates/boon/src/platform/browser/bridge_v2.rs` |

### Documentation References

| Topic | File |
|-------|------|
| Node identification | `docs/new_boon/2.1_NODE_IDENTIFICATION.md` |
| Arena memory | `docs/new_boon/2.2_ARENA_MEMORY.md` |
| Message passing | `docs/new_boon/2.3_MESSAGE_PASSING.md` |
| Event loop | `docs/new_boon/2.4_EVENT_LOOP.md` |
| Bridge API | `docs/new_boon/2.8_BRIDGE_API.md` |
| Issues | `docs/new_boon/6.1_ISSUES.md` |
| Examples | `docs/new_boon/6.2_EXAMPLES.md` |

---

**Last Updated:** Created for M0/M1 implementation
