# M0 + M1 Implementation Checklist

**Purpose:** Agent-executable checklist for implementing M0 (CLI) and M1 (New Engine in Playground).

**Progress Legend:**
- `[ ]` Not started
- `[~]` In progress
- `[x]` Complete

### Realistic Time Estimates

| Phase | Duration | Notes |
|-------|----------|-------|
| Phase 1-3 | 2 weeks | Core types, arena, basic nodes |
| Phase 3.5 | 1 week | CLI test harness (enables fast iteration) |
| Phase 4 | 2 weeks | Combinators (LATEST, HOLD, THEN, WHEN, WHILE) |
| Phase 4.5 | 1 week | FUNCTION + BLOCK compilation |
| Phase 5 | 2 weeks | Lists with Issue 16/17 fixes |
| Phase 6 | 2 weeks | Timer, events, effects |
| Phase 7a | 1 week | Basic bridge |
| Phase 7b | 1 week | Element API |
| Phase 7c | 2 weeks | Interactive features, LINK, events |
| Phase 7d | 1 week | PASS/PASSED, TextTemplate |
| Phase 8 | 1 week | Snapshot system |
| Phase 9 | 1 week | CLI completion |
| **Total** | **16-18 weeks** | Full-time, with debugging/iteration |

**Task granularity:** ~150 atomic tasks, each 15-60 minutes

**Risk factors:**
- Integration issues between phases (+1-2 weeks)
- Unforeseen edge cases in LINK/WHILE interaction (+1 week)
- Performance optimization if needed (+1 week)

### Critical Issues Phase Assignment

| Issue # | Description | Phase | Checklist Section |
|---------|-------------|-------|-------------------|
| **Issue 4** | TextTemplate reactive updates | Phase 7d | §9.12 |
| **Issue 7** | Element states (hovered, focused) | Phase 7c | §9.8 |
| **Issue 15** | LINK bind/unbind protocol | Phase 7c | §9.7 |
| **Issue 16** | List/map external dependencies | Phase 5 | §7.3.3 |
| **Issue 17** | Chained List/remove | Phase 5 | §7.3.2 |

All critical issues are now assigned to specific phases with explicit verification tests.

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
  - `docs/new_boon/2.3_MESSAGE_PASSING.md` - Message, Payload, Node kinds, routing table management
  - `docs/new_boon/2.4_EVENT_LOOP.md` - EventLoop, tick processing
  - `docs/new_boon/2.6_ERROR_HANDLING.md` - FLUSH semantics, FLUSH + HOLD interaction
  - `docs/new_boon/2.8_BRIDGE_API.md` - Bridge and DOM reconciliation
  - `docs/new_boon/2.11_CONTEXT_PASSING.md` - PASS/PASSED context threading
  - `docs/new_boon/6.1_ISSUES.md` - Known issues to fix
  - `docs/new_boon/plans/M0_CLI__M1_NEW_ENGINE_IN_PLAYGROUND.md` - Canonical implementation plan

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

    impl Port {
        /// Extract the input index from an Input port, panics for other variants.
        pub fn input_index(&self) -> usize {
            match self {
                Port::Input(i) => *i as usize,
                _ => panic!("input_index() called on non-Input port: {:?}", self),
            }
        }
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
    /// 64-byte cache-line aligned for performance (see §2.2.5).
    #[repr(C, align(64))]
    pub struct ReactiveNode {
        pub generation: u32,
        pub version: u32,
        pub dirty: bool,
        pub kind_tag: u8,
        pub input_count: u8,
        pub subscriber_count: u8,
        pub inputs: [SlotId; 4],
        pub subscribers: [SlotId; 2],
        pub extension: Option<Box<NodeExtension>>,
    }

    /// Type alias: NodeKindData is the same as NodeKind enum (see node.rs).
    /// The name "NodeKindData" emphasizes this is the data stored in extension,
    /// while "NodeKind" is used for matching/dispatch.
    pub type NodeKindData = NodeKind;

    /// Heap-allocated extension for value storage and overflow arrays.
    pub struct NodeExtension {
        pub current_value: Option<Payload>,
        pub pending_deltas: Vec<Payload>,
        pub kind_data: NodeKindData,  // See NodeKind enum in node.rs
        pub extra_inputs: Vec<SlotId>,
        pub extra_subscribers: Vec<SlotId>,
    }

    impl Default for ReactiveNode {
        fn default() -> Self {
            Self {
                generation: 0,
                version: 0,
                dirty: false,
                kind_tag: 0,
                input_count: 0,
                subscriber_count: 0,
                inputs: [SlotId::INVALID; 4],
                subscribers: [SlotId::INVALID; 2],
                extension: None,
            }
        }
    }

    impl ReactiveNode {
        /// Get the node kind from extension (lazy allocation means this may be None).
        pub fn kind(&self) -> Option<&NodeKind> {
            self.extension.as_ref().map(|ext| &ext.kind_data)
        }

        /// Get mutable reference to node kind (for in-place updates).
        pub fn kind_mut(&mut self) -> Option<&mut NodeKind> {
            self.extension.as_mut().map(|ext| &mut ext.kind_data)
        }

        /// Get mutable access to extension, allocating if needed (lazy allocation §2.2.5.1).
        pub fn extension_mut(&mut self) -> &mut NodeExtension {
            self.extension.get_or_insert_with(|| Box::new(NodeExtension::default()))
        }

        /// Set the node kind (allocates extension if needed).
        pub fn set_kind(&mut self, kind: NodeKind) {
            self.extension_mut().kind_data = kind;
        }
    }

    impl Default for NodeExtension {
        fn default() -> Self {
            Self {
                current_value: None,
                pending_deltas: Vec::new(),
                kind_data: NodeKind::Wire { source: None },  // Default to empty wire
                extra_inputs: Vec::new(),
                extra_subscribers: Vec::new(),
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
        /// Intern table: FieldId → field name string (see §2.3.1.1)
        field_names: HashMap<u32, Arc<str>>,
        /// Reverse lookup: field name → FieldId
        field_ids: HashMap<Arc<str>, u32>,
        /// Next FieldId to allocate
        next_field_id: u32,
        /// Intern table: TagId → tag name string
        tag_names: HashMap<u32, Arc<str>>,
        /// Reverse lookup: tag name → TagId
        tag_ids: HashMap<Arc<str>, u32>,
        /// Next TagId to allocate
        next_tag_id: u32,
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
                field_names: HashMap::new(),
                field_ids: HashMap::new(),
                next_field_id: 0,
                tag_names: HashMap::new(),
                tag_ids: HashMap::new(),
                next_tag_id: 0,
            }
        }

        /// Intern a field name, returning its FieldId.
        pub fn intern_field(&mut self, name: &str) -> u32 {
            if let Some(&id) = self.field_ids.get(name) {
                return id;
            }
            let id = self.next_field_id;
            self.next_field_id += 1;
            let name: Arc<str> = name.into();
            self.field_names.insert(id, name.clone());
            self.field_ids.insert(name, id);
            id
        }

        /// Get field name from FieldId.
        pub fn get_field_name(&self, id: u32) -> Option<&Arc<str>> {
            self.field_names.get(&id)
        }

        /// Intern a tag name, returning its TagId.
        pub fn intern_tag(&mut self, name: &str) -> u32 {
            if let Some(&id) = self.tag_ids.get(name) {
                return id;
            }
            let id = self.next_tag_id;
            self.next_tag_id += 1;
            let name: Arc<str> = name.into();
            self.tag_names.insert(id, name.clone());
            self.tag_ids.insert(name, id);
            id
        }

        /// Get tag name from TagId.
        pub fn get_tag_name(&self, id: u32) -> Option<&Arc<str>> {
            self.tag_names.get(&id)
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
  jj new -m "Phase 1: Core types and arena for engine_v2"
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
        Insert { key: ItemKey, index: u32, value: Payload },    // Add item at index
        Update { key: ItemKey, value: Payload },                 // Replace item value
        FieldUpdate { key: ItemKey, field: FieldId, value: Payload }, // Nested field within item
        Remove { key: ItemKey },                                 // Remove item by key
        Move { key: ItemKey, from_index: u32, to_index: u32 },   // Reorder item
        Replace { items: Vec<(ItemKey, Payload)> },              // Full list replacement
    }
    ```
  - Verify: `cargo check -p boon`

- [ ] **2.1.2** Add ObjectDelta to message.rs
  - Content:
    ```rust
    /// Field identifier - interned string index for O(1) lookup.
    ///
    /// **Engine representation:** `u32` index into global intern table
    /// **Protocol JSON:** String field name (human-readable, see §6.8)
    /// **Persistence:** Intern table serialized alongside snapshot
    ///
    /// Use `intern_table.get_name(field_id)` to recover the string.
    /// Use `intern_table.intern("field_name")` to get the FieldId.
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

        // Route from source's output to target's input port 0
        el.routing.add_route(source, target, Port::Input(0));
        el.deliver_message(source, Payload::Number(42.0));

        assert_eq!(el.dirty_nodes.len(), 1);
        assert_eq!(el.dirty_nodes[0].slot, target);
        assert_eq!(el.dirty_nodes[0].port, Port::Input(0));
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
  jj new -m "Phase 2: Message passing and routing table"
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

        /// Test probe - stores last received value for assertions (test-only)
        #[cfg(test)]
        Probe { last: Option<Payload> },
    }
    ```
  - Verify: `cargo check -p boon`

### 3.2 Node Processing

**Note:** Add Probe handling to process_node for test observability:

- [ ] **3.2.1** Implement process_node in EventLoop
  - Content:
    ```rust
    impl EventLoop {
        fn process_node(&mut self, entry: DirtyEntry) {
            // Take the message from inbox (if any) - see §4.6.3
            let msg = self.inbox.remove(&(entry.slot, entry.port));

            let Some(node) = self.arena.get(entry.slot) else {
                return;
            };

            // Access kind through extension (see §2.2.5.1 lazy allocation)
            let Some(kind) = node.kind() else {
                return;  // Node has no extension yet (shouldn't happen for dirty nodes)
            };

            let output = match kind {
                NodeKind::Producer { value } => value.clone(),
                NodeKind::Wire { source } => {
                    source.and_then(|s| {
                        self.arena.get(s).and_then(|n| {
                            n.kind().and_then(|k| match k {
                                NodeKind::Producer { value } => value.clone(),
                                _ => None,
                            })
                        })
                    })
                }
                NodeKind::Router { .. } => None, // Router doesn't emit directly
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
                self.deliver_message(entry.slot, payload);
            }
        }
    }
    ```
  - Verify: `cargo check -p boon`

- [ ] **3.2.2** Add test helper for creating Probe nodes
  - Content (in test module):
    ```rust
    #[cfg(test)]
    impl CompileContext<'_> {
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
    ```
  - Verify: `cargo test -p boon engine_v2`

### 3.3 Create Basic Evaluator

- [ ] **3.3.1** Create `evaluator_v2/mod.rs`
  - File: `crates/boon/src/evaluator_v2/mod.rs`
  - Content:
    ```rust
    //! Evaluator for the new arena-based engine.
    //!
    //! Compiles AST expressions into reactive nodes in the arena.

    use std::collections::HashMap;
    use crate::engine_v2::{
        arena::SlotId,
        event_loop::EventLoop,
        message::Payload,
        node::NodeKind,
        address::ScopeId,
    };

    /// Context for compilation.
    /// Tracks compile-time state for expression compilation.
    pub struct CompileContext<'a> {
        pub event_loop: &'a mut EventLoop,
        /// Current scope for node instantiation
        pub scope_id: ScopeId,
        /// Local bindings: variable name → SlotId (lexical scope)
        pub local_bindings: HashMap<String, SlotId>,
        /// PASS/PASSED context stack (§2.11 - compile-time only)
        pub pass_stack: Vec<SlotId>,
        /// Function parameters: name → SlotId (for FUNCTION compilation)
        pub parameters: HashMap<String, SlotId>,
    }

    impl<'a> CompileContext<'a> {
        pub fn new(event_loop: &'a mut EventLoop) -> Self {
            Self {
                event_loop,
                scope_id: ScopeId::ROOT,
                local_bindings: HashMap::new(),
                pass_stack: Vec::new(),
                parameters: HashMap::new(),
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
                node.set_kind(NodeKind::Producer { value: Some(value) });
            }
            slot
        }

        /// Compile a wire (alias to another slot).
        pub fn compile_wire(&mut self, source: SlotId) -> SlotId {
            let slot = self.event_loop.arena.alloc();
            if let Some(node) = self.event_loop.arena.get_mut(slot) {
                node.set_kind(NodeKind::Wire { source: Some(source) });
            }
            // Subscribe to source (wire receives on its input port 0)
            self.event_loop.routing.add_route(source, slot, crate::engine_v2::address::Port::Input(0));
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
                node.set_kind(NodeKind::Router { fields: field_map });
            }

            slot
        }

        /// Get a field from a Router node.
        pub fn get_field(&self, router: SlotId, field: FieldId) -> Option<SlotId> {
            let node = self.event_loop.arena.get(router)?;
            match node.kind()? {
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
  jj new -m "Phase 3: Basic nodes (Producer, Wire, Router) and evaluator"
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
  - File: `crates/boon/src/platform.rs`
  - Edit: Add the following (do NOT create `platform/mod.rs` - it would conflict with `platform.rs`):
    ```rust
    pub mod browser;
    #[cfg(feature = "cli")]
    pub mod cli;
    ```
  - File: `crates/boon/src/lib.rs`
  - Verify: `pub mod platform;` already exists; no edit needed
  - Verify: `cargo check -p boon`

### 4.2 Value Materialization

- [ ] **4.2.1** Add materialize function to message.rs
  - Note: Gate with `#[cfg(feature = "cli")]` so core engine_v2 doesn't require serde_json
  - Content:
    ```rust
    #[cfg(feature = "cli")]
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
  - Verify: `cargo check -p boon` (without cli feature)
  - Verify: `cargo check -p boon --features cli` (with cli feature)

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
  - Note: No workspace edit needed; root Cargo.toml already has `members = ["crates/*"]` which auto-includes new crates
  - Verify: `cargo check -p boon-cli`

### 4.4 Add cli Feature to boon

- [ ] **4.4.1** Add cli feature to boon's Cargo.toml
  - File: `crates/boon/Cargo.toml`
  - Edit: Add optional dependency:
    ```toml
    [dependencies]
    # ... existing deps ...
    serde_json = { version = "1", optional = true }
    ```
  - Edit: Update features (preserving existing defaults):
    ```toml
    [features]
    default = ["debug-channels"]
    debug-channels = []
    cli = ["dep:serde_json"]
    ```
  - Note: Do NOT replace `default = ["debug-channels"]` with `default = []`
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
  jj new -m "Phase 3.5: CLI test harness foundation"
  ```

### 4.6 Message Inbox & Current Value API

**Note:** Phase 4/5 assumes message handling but the earlier EventLoop only has dirty_nodes and no message storage. This subsection adds the required API before combinators.

- [ ] **4.6.1** Add inbox to EventLoop
  - File: `crates/boon/src/engine_v2/event_loop.rs`
  - Content: Add to EventLoop struct:
    ```rust
    use std::collections::HashMap;

    pub struct EventLoop {
        // ... existing fields ...
        /// Inbox: stores pending messages by (target_slot, target_port)
        pub inbox: HashMap<(SlotId, Port), Payload>,
    }

    impl EventLoop {
        pub fn new() -> Self {
            Self {
                // ... existing fields ...
                inbox: HashMap::new(),
            }
        }
    }
    ```
  - Verify: `cargo check -p boon`

- [ ] **4.6.2** Update deliver_message to store in inbox
  - Content: Modify deliver_message to store payload before marking dirty:
    ```rust
    impl EventLoop {
        /// Deliver a message to all subscribers of a source node.
        pub fn deliver_message(&mut self, source: SlotId, payload: Payload) {
            let subscribers: Vec<_> = self.routing
                .get_subscribers(source)
                .to_vec();

            for (target, port) in subscribers {
                // Store the payload for the target to consume
                self.inbox.insert((target, port), payload.clone());
                self.mark_dirty(target, port);
            }
        }
    }
    ```
  - Verify: `cargo check -p boon`

- [ ] **4.6.3** Update process_node to consume from inbox
  - Content: process_node should take/remove payload from inbox:
    ```rust
    fn process_node(&mut self, entry: DirtyEntry) {
        // Take the message from inbox (if any)
        let msg = self.inbox.remove(&(entry.slot, entry.port));

        let Some(node) = self.arena.get(entry.slot) else {
            return;
        };

        // ... rest of processing uses `msg` as Option<Payload> ...
    }
    ```
  - Note: For nodes expecting messages (Combiner, Register, etc.), `msg` should be `Some`
  - Verify: `cargo check -p boon`

- [ ] **4.6.4** Add get_current_value helper
  - Content: Add method to get stored current value from a node:
    ```rust
    impl EventLoop {
        /// Get the current value stored in a node's extension.
        pub fn get_current_value(&self, slot: SlotId) -> Option<&Payload> {
            self.arena.get(slot)
                .and_then(|node| node.extension.as_ref())
                .and_then(|ext| ext.current_value.as_ref())
        }

        /// Set the current value for a node.
        pub fn set_current_value(&mut self, slot: SlotId, value: Payload) {
            if let Some(node) = self.arena.get_mut(slot) {
                node.extension_mut().current_value = Some(value);
            }
        }
    }
    ```
  - Note: `current_value` is stored in `NodeExtension.current_value` (see arena.rs)
  - Verify: `cargo check -p boon`

- [ ] **4.6.5** Add to_display_string helper for Payload
  - Content: Add method to Payload for string formatting (used by TextTemplate):
    ```rust
    impl Payload {
        /// Convert payload to display string for text interpolation.
        pub fn to_display_string(&self) -> String {
            match self {
                Payload::Number(n) => n.to_string(),
                Payload::Text(s) => s.to_string(),
                Payload::Bool(b) => b.to_string(),
                Payload::Unit => String::new(),
                Payload::Tag(t) => format!("Tag({})", t),
                Payload::TaggedObject { tag, .. } => format!("TaggedObject({})", tag),
                Payload::ListHandle(_) => "[list]".to_string(),
                Payload::ObjectHandle(_) => "{object}".to_string(),
                Payload::Flushed(inner) => format!("Error: {}", inner.to_display_string()),
                Payload::ListDelta(_) => "[delta]".to_string(),
                Payload::ObjectDelta(_) => "{delta}".to_string(),
            }
        }
    }
    ```
  - Verify: `cargo check -p boon`

- [ ] **4.6.6** Run tests
  ```bash
  cargo test -p boon engine_v2
  ```
  - Expected: All tests pass

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

### 5.6 Implement Combiner (LATEST) Processing

- [ ] **5.6.1** Implement Combiner message handling
  - File: `crates/boon/src/engine_v2/event_loop.rs`
  - Content:
    ```rust
    fn process_combiner(&mut self, slot: SlotId, port: Port, msg: Payload) {
        let node = self.arena.get_mut(slot);
        if let NodeKind::Combiner { inputs, last_values } = &mut node.kind {
            // Update the specific input's cached value
            let input_idx = port.input_index();
            last_values[input_idx] = Some(msg);

            // Check if all inputs have values
            if last_values.iter().all(|v| v.is_some()) {
                // Emit combined output (object or list of values)
                let combined = self.combine_values(last_values);
                self.emit_to_subscribers(slot, combined);
            }
        }
    }
    ```

- [ ] **5.6.2** Implement combine_values helper
  - Options: a) List of values, b) Object with named fields, c) Last arriving value
  - Decision: Use last-value semantics (LATEST emits the most recent value)

- [ ] **5.6.3** Implement dirty tracking for Combiner
  - Combiner is dirty when any input arrives
  - Emit output when all inputs have at least one value

### 5.7 Implement Register (HOLD) Processing

- [ ] **5.7.1** Implement Register initial value handling
  - Store initial value on first tick
  - Emit initial value to subscribers immediately

- [ ] **5.7.2** Implement Register body subscription
  - Register subscribes to its body subgraph output
  - Body receives piped input values

- [ ] **5.7.3** Implement Register state update
  - Content:
    ```rust
    fn process_register(&mut self, slot: SlotId, msg: Payload) {
        let node = self.arena.get_mut(slot);
        if let NodeKind::Register { stored_value, .. } = &mut node.kind {
            match msg {
                Payload::Flushed(err) => {
                    // FLUSH + HOLD: Don't store error, propagate it
                    self.emit_to_subscribers(slot, Payload::Flushed(err));
                }
                value => {
                    // Store new state and emit
                    *stored_value = Some(value.clone());
                    self.emit_to_subscribers(slot, value);
                }
            }
        }
    }
    ```

- [ ] **5.7.4** Implement `state` binding resolution in HOLD body
  - Body expressions can reference `state` to get current value
  - Create wire that reads from Register's stored_value

### 5.8 Implement Transformer (THEN) Processing

- [ ] **5.8.1** Implement THEN trigger on input arrival
  - When input arrives, evaluate body expression
  - Emit body result to subscribers

- [ ] **5.8.2** Implement THEN with nested expressions
  - Body may contain LATEST, WHEN, etc.
  - Create subgraph for body, connect output

- [ ] **5.8.3** Implement SKIP handling in THEN
  - If body evaluates to SKIP, don't emit (filter pattern)

### 5.9 Implement PatternMux (WHEN) Processing

- [ ] **5.9.1** Implement WHEN pattern matching
  - On input arrival, match against arm patterns
  - First matching arm's body is evaluated

- [ ] **5.9.2** Implement literal pattern matching
  - Numbers: exact match
  - Tags: exact match
  - `__` (wildcard): always matches

- [ ] **5.9.3** Implement binding patterns in WHEN
  - `Tag name =>` binds `name` to inner value
  - `n =>` binds entire value to `n`

- [ ] **5.9.4** Implement WHEN fallthrough to SKIP
  - If no pattern matches, treat as SKIP (don't emit)

### 5.10 Implement SwitchedWire (WHILE) Processing

- [ ] **5.10.1** Implement WHILE arm switching
  - On input arrival, determine which arm matches
  - If arm changes, finalize old arm scope, create new

- [ ] **5.10.2** Implement arm scope creation
  - Each arm has its own scope with bindings
  - Content:
    ```rust
    fn switch_while_arm(&mut self, slot: SlotId, new_arm: usize) {
        let node = self.arena.get_mut(slot);
        if let NodeKind::SwitchedWire { current_arm, arm_outputs, .. } = &mut node.kind {
            if *current_arm != Some(new_arm) {
                // Finalize old arm scope
                if let Some(old) = *current_arm {
                    self.pending_finalizations.push(arm_outputs[old]);
                }
                // Create new arm scope
                *current_arm = Some(new_arm);
                // Wire up new arm's routes
            }
        }
    }
    ```

- [ ] **5.10.3** Implement continuous value forwarding
  - WHILE continuously forwards values from active arm
  - Re-evaluate arm body when upstream values change

### 5.11 Add Combinator Compilation

- [ ] **5.11.1** Add compile_latest to CompileContext
  - Parse LATEST { expr1, expr2, ... } arms
  - Allocate Combiner node with input count = arm count
  - Wire each expression's output to Combiner input port

- [ ] **5.11.2** Add compile_hold to CompileContext
  - Parse `initial |> HOLD state { body }`
  - Allocate Register node
  - Create body subgraph with `state` binding to Register's value
  - Wire body output to Register's body_input

- [ ] **5.11.3** Add compile_then to CompileContext
  - Parse `input |> THEN { body }`
  - Allocate Transformer node
  - Create body subgraph, wire to Transformer

- [ ] **5.11.4** Add compile_when to CompileContext
  - Parse `input |> WHEN { pattern1 => body1, ... }`
  - Allocate PatternMux node
  - Create arm bodies as subgraphs
  - Store patterns for runtime matching

- [ ] **5.11.5** Add compile_while to CompileContext
  - Parse `input |> WHILE { pattern1 => body1, ... }`
  - Allocate SwitchedWire node
  - Defer arm body creation (lazy on match)

### 5.12 Phase 4 Verification

**Note:** Use Probe nodes to make assertions (not just "dirty_nodes got shorter").

- [ ] **5.12.1** Unit test: LATEST with two constants
  ```rust
  #[test]
  fn test_latest_two_constants() {
      let mut el = EventLoop::new();
      let mut ctx = CompileContext::new(&mut el);

      let a = ctx.compile_constant(Payload::Number(1.0));
      let b = ctx.compile_constant(Payload::Number(2.0));
      let latest = ctx.compile_latest(vec![a, b]);

      // Connect probe to observe output
      let probe = ctx.compile_probe();
      ctx.event_loop.routing.add_route(latest, probe, Port::Input(0));

      // Run until quiescent
      ctx.event_loop.run_tick();

      // Expect: last value wins (2.0)
      let value = ctx.get_probe_value(probe);
      assert!(matches!(value, Some(Payload::Number(n)) if n == 2.0));
  }
  ```

- [ ] **5.12.2** Unit test: HOLD counter pattern
  ```rust
  #[test]
  fn test_hold_counter() {
      // count: 0 |> HOLD state { trigger |> THEN { state + 1 } }
      let mut el = EventLoop::new();
      let mut ctx = CompileContext::new(&mut el);

      // Setup hold with initial value 0
      let hold = ctx.compile_hold_counter(0.0);
      let probe = ctx.compile_probe();
      ctx.event_loop.routing.add_route(hold, probe, Port::Input(0));

      // Trigger 3 times
      for _ in 0..3 {
          ctx.event_loop.deliver_message(hold, Payload::Unit); // trigger
          ctx.event_loop.run_tick();
      }

      // Expect final value = 3
      let value = ctx.get_probe_value(probe);
      assert!(matches!(value, Some(Payload::Number(n)) if n == 3.0));
  }
  ```

- [ ] **5.12.3** Unit test: WHEN pattern matching
  ```rust
  #[test]
  fn test_when_pattern_match() {
      // input |> WHEN { 1 => "one", 2 => "two", __ => "other" }
      let mut el = EventLoop::new();
      let mut ctx = CompileContext::new(&mut el);

      let when_node = ctx.compile_when_example();
      let probe = ctx.compile_probe();
      ctx.event_loop.routing.add_route(when_node, probe, Port::Input(0));

      // Send 1
      ctx.event_loop.deliver_message(when_node, Payload::Number(1.0));
      ctx.event_loop.run_tick();

      // Expect "one"
      let value = ctx.get_probe_value(probe);
      assert!(matches!(value, Some(Payload::Text(s)) if s.as_ref() == "one"));
  }
  ```

- [ ] **5.12.4** Unit test: WHILE arm switching
  ```rust
  #[test]
  fn test_while_arm_switch() {
      // toggle |> WHILE { True => "on", False => "off" }
      let mut el = EventLoop::new();
      let mut ctx = CompileContext::new(&mut el);

      let while_node = ctx.compile_while_toggle();
      let probe = ctx.compile_probe();
      ctx.event_loop.routing.add_route(while_node, probe, Port::Input(0));

      // Send True
      ctx.event_loop.deliver_message(while_node, Payload::Bool(true));
      ctx.event_loop.run_tick();
      assert!(matches!(ctx.get_probe_value(probe), Some(Payload::Text(s)) if s.as_ref() == "on"));

      // Send False
      ctx.event_loop.deliver_message(while_node, Payload::Bool(false));
      ctx.event_loop.run_tick();
      assert!(matches!(ctx.get_probe_value(probe), Some(Payload::Text(s)) if s.as_ref() == "off"));
  }
  ```

- [ ] **5.12.5** Unit test: FLUSH in HOLD body
  ```rust
  #[test]
  fn test_flush_in_hold() {
      // Verify FLUSH propagates to output but doesn't corrupt state (§2.6.5)
      let mut el = EventLoop::new();
      let mut ctx = CompileContext::new(&mut el);

      let hold = ctx.compile_hold_with_potential_flush();
      let probe = ctx.compile_probe();
      ctx.event_loop.routing.add_route(hold, probe, Port::Input(0));

      // Trigger FLUSH
      ctx.event_loop.deliver_message(hold, Payload::Flushed(Box::new(Payload::Text("error".into()))));
      ctx.event_loop.run_tick();

      // FLUSH should propagate to output
      assert!(matches!(ctx.get_probe_value(probe), Some(Payload::Flushed(_))));

      // State should remain valid (not corrupted by FLUSH)
      let state_value = ctx.get_hold_state(hold);
      assert!(!matches!(state_value, Some(Payload::Flushed(_))));
  }
  ```

- [ ] **5.12.6** Run all tests
  ```bash
  cargo test -p boon engine_v2
  cargo test -p boon evaluator_v2
  ```

- [ ] **5.12.7** Commit Phase 4
  ```bash
  jj new -m "Phase 4: Combinators (LATEST, HOLD, THEN, WHEN, WHILE)"
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
**Goal:** Connect new engine to Zoon UI. Split into 4 sub-phases for manageability.

**Estimated Duration:** 4-5 weeks total (previously underestimated as 1 week)

---

### Phase 7a: Basic Bridge (Week 1)

**Goal:** Minimal bridge connecting arena values to Zoon.

#### 9.1 Bridge Module

- [ ] **9.1.1** Create `platform/browser/bridge_v2.rs`
- [ ] **9.1.2** Implement Payload → Zoon primitive conversion
  - Number → RawText (or formatted display)
  - Text → RawText
  - Bool → RawText ("true"/"false")
  - Unit → empty
- [ ] **9.1.3** Implement ObjectHandle → Zoon element conversion
  - Router node → child elements
- [ ] **9.1.4** Handle scalar roots (Text, Number, Bool, Unit) - see §2.8

#### 9.2 Feature Flag

- [ ] **9.2.1** Add `engine-v2` feature flag to Cargo.toml
- [ ] **9.2.2** Wire interpreter to conditionally use new engine
- [ ] **9.2.3** Ensure old engine still works (feature off)

#### 9.3 Basic Document/new

- [ ] **9.3.1** Implement Document/new for scalar roots
- [ ] **9.3.2** Test: `document: TEXT { Hello } |> Document/new()`
- [ ] **9.3.3** Test: `document: 42 |> Document/new()`

**Phase 7a Verification:**
- [ ] **9.3.4** Simple TEXT example renders in playground
- [ ] **9.3.5** Commit Phase 7a

---

### Phase 7b: Element API (Week 2)

**Goal:** Implement Element/* functions for building UIs.

#### 9.4 Core Element Functions

- [ ] **9.4.1** Implement Element/container (basic wrapper)
- [ ] **9.4.2** Implement Element/label (text display)
- [ ] **9.4.3** Implement Element/button (clickable, no events yet)
- [ ] **9.4.4** Implement Element/stripe (flex layout, direction/gap)
- [ ] **9.4.5** Implement Element/stack (z-order layout)

#### 9.5 Styling Support

- [ ] **9.5.1** Parse style object from Boon object
- [ ] **9.5.2** Map Boon styles to Zoon properties
  - width, height, padding, font, background, etc.
- [ ] **9.5.3** Handle Oklch color values
- [ ] **9.5.4** Handle Duration values (for animations)

#### 9.6 More Elements

- [ ] **9.6.1** Implement Element/text_input (no events yet)
- [ ] **9.6.2** Implement Element/checkbox (no events yet)
- [ ] **9.6.3** Implement Element/paragraph
- [ ] **9.6.4** Implement Element/link

**Phase 7b Verification:**
- [ ] **9.6.5** Static counter UI renders (no reactivity)
- [ ] **9.6.6** Commit Phase 7b

---

### Phase 7c: Interactive Features (Weeks 3-4)

**Goal:** Add event handling, LINK protocol, and element states.

#### 9.7 LINK Protocol (Issue 15)

- [ ] **9.7.1** Implement IOPad node for LINK
- [ ] **9.7.2** Implement LINK bind (attach event listeners)
- [ ] **9.7.3** Implement LINK unbind (detach listeners on scope finalization)
- [ ] **9.7.4** Implement scope finalization at tick end
- [ ] **9.7.5** Test: switch_hold_test.bn (LINK in WHILE arms)

#### 9.8 Element States (Issue 7)

- [ ] **9.8.1** Add ElementState struct (hovered, focused slots)
- [ ] **9.8.2** Wire hovered state to mouse events
- [ ] **9.8.3** Wire focused state to focus/blur events
- [ ] **9.8.4** Test: button_hover_test.bn

#### 9.9 Element Events

- [ ] **9.9.1** Implement element.event.press (button click)
- [ ] **9.9.2** Implement element.event.change (input change)
- [ ] **9.9.3** Implement element.event.key_down (key press)
- [ ] **9.9.4** Implement element.event.blur, element.event.double_click

#### 9.10 Router Effects

- [ ] **9.10.1** Implement Router/go_to effect
- [ ] **9.10.2** Implement Router/route query
- [ ] **9.10.3** Test: pages.bn

**Phase 7c Verification:**
- [ ] **9.10.4** counter.bn works with button click
- [ ] **9.10.5** Commit Phase 7c

---

### Phase 7d: Context & Templates (Week 5)

**Goal:** PASS/PASSED context and reactive text templates.

#### 9.11 PASS/PASSED Context (§2.11)

**Note:** Uses types from `crates/boon/src/parser/static_expression.rs`:
- `Expression::FunctionCall { path, arguments }` for function calls
- `Argument { name, is_referenced, value }` for call arguments
- `Alias::WithPassed { extra_parts }` for PASSED.field.path

- [ ] **9.11.0** Add CompileError enum to evaluator_v2
  - File: `crates/boon/src/evaluator_v2/mod.rs`
  - Content:
    ```rust
    /// Compilation errors for the new evaluator.
    #[derive(Debug, Clone)]
    pub enum CompileError {
        /// PASSED used outside of a PASS: context
        PassedNotAvailable,
        /// Unknown variable reference
        UnknownVariable(String),
        /// Unknown function
        UnknownFunction(Vec<String>),
        /// Type mismatch during compilation
        TypeMismatch { expected: String, got: String },
    }

    impl std::fmt::Display for CompileError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                CompileError::PassedNotAvailable => {
                    write!(f, "PASSED used outside of PASS: context")
                }
                CompileError::UnknownVariable(name) => {
                    write!(f, "Unknown variable: {}", name)
                }
                CompileError::UnknownFunction(path) => {
                    write!(f, "Unknown function: {}", path.join("/"))
                }
                CompileError::TypeMismatch { expected, got } => {
                    write!(f, "Type mismatch: expected {}, got {}", expected, got)
                }
            }
        }
    }

    impl std::error::Error for CompileError {}
    ```
  - Verify: `cargo check -p boon`

- [ ] **9.11.1** Verify pass_stack in CompileContext
  - Already added in §3.3.1: `pass_stack: Vec<SlotId>` with `push_pass()`, `pop_pass()`, `current_passed()`

- [ ] **9.11.2** Implement PASS: argument compilation
  - Uses: `Expression::FunctionCall { path, arguments }` and `Argument` from static_expression
  - Content:
    ```rust
    use crate::parser::static_expression::{Expression, Argument, Spanned};

    /// Compile function call with PASS argument (§2.11).
    /// Uses Expression::FunctionCall from static_expression.rs.
    pub fn compile_function_call(
        &mut self,
        path: &[StrSlice],
        arguments: &[Spanned<Argument>],
    ) -> Result<SlotId, CompileError> {
        // Process named arguments as local bindings
        for arg in arguments {
            let arg_name = arg.node.name.as_str();
            if arg_name != "PASS" {
                if let Some(ref value) = arg.node.value {
                    let arg_slot = self.compile_expression(&value.node)?;
                    self.add_local_binding(arg_name.to_string(), arg_slot);
                }
            }
        }

        // Handle PASS argument
        let has_pass = arguments.iter().any(|a| a.node.name.as_str() == "PASS");
        if let Some(pass_arg) = arguments.iter().find(|a| a.node.name.as_str() == "PASS") {
            if let Some(ref value) = pass_arg.node.value {
                let pass_slot = self.compile_expression(&value.node)?;
                self.push_pass(pass_slot);
            }
        }

        // Resolve and inline function body
        let result = self.inline_function_body(path)?;

        // Pop PASS context if we pushed one
        if has_pass {
            self.pop_pass();
        }

        // Clean up local bindings
        for arg in arguments {
            let arg_name = arg.node.name.as_str();
            if arg_name != "PASS" {
                self.remove_local_binding(arg_name);
            }
        }

        Ok(result)
    }
    ```

- [ ] **9.11.3** Implement PASSED keyword resolution
  - Uses: `Alias::WithPassed { extra_parts }` from static_expression for PASSED.field.path
  - Content:
    ```rust
    use crate::parser::static_expression::StrSlice;

    /// Compile PASSED or PASSED.field.path expression.
    /// extra_parts: the field path after PASSED (e.g., ["store", "items"] for PASSED.store.items)
    pub fn compile_passed(&mut self, extra_parts: &[StrSlice]) -> Result<SlotId, CompileError> {
        let pass_slot = self.current_passed()
            .ok_or(CompileError::PassedNotAvailable)?;

        if extra_parts.is_empty() {
            // Just `PASSED` - return the entire context
            return Ok(pass_slot);
        }

        // `PASSED.store.items` - emit field access chain
        let mut current = pass_slot;
        for part in extra_parts {
            let field_id = self.event_loop.arena.intern_field(part.as_str());
            current = self.compile_field_access(current, field_id);
        }

        Ok(current)
    }
    ```

- [ ] **9.11.4** Implement pass_stack push/pop at function boundaries
  - Already implemented in push_pass/pop_pass methods

- [ ] **9.11.5** Test: nested function calls with PASS

#### 9.12 TextTemplate Node (Issue 4)

- [ ] **9.12.1** Add TextTemplate to NodeKind
  - Content:
    ```rust
    /// Text template with reactive interpolations (TEXT { ... {var} ... })
    TextTemplate {
        /// Template string with placeholders: "Count: {0}, Total: {1}"
        template: String,
        /// Dependencies: SlotIds referenced in interpolations (collected at compile-time)
        dependencies: Vec<SlotId>,
        /// Cached rendered string (updated when any dependency changes)
        cached: Option<Arc<str>>,
    },
    ```

- [ ] **9.12.2** Implement compile-time dependency collection (§2.3)
  - Uses: `TextPart` from `crates/boon/src/parser/static_expression.rs`:
    ```rust
    pub enum TextPart {
        Text(StrSlice),
        Interpolation { var: StrSlice, referenced_span: Option<SimpleSpan> },
    }
    ```
  - Content:
    ```rust
    use crate::parser::static_expression::TextPart;

    /// Compile TEXT { literal {var} more {var2} } into TextTemplate node.
    /// Uses TextPart from static_expression.rs.
    pub fn compile_text_template(&mut self, parts: &[TextPart]) -> Result<SlotId, CompileError> {
        let mut template_parts = Vec::new();
        let mut dependencies = Vec::new();

        for part in parts {
            match part {
                TextPart::Text(text) => {
                    template_parts.push(text.as_str().to_string());
                }
                TextPart::Interpolation { var, referenced_span } => {
                    // Resolve the variable to a SlotId
                    let var_name = var.as_str();
                    let dep_slot = self.resolve_variable(var_name)
                        .ok_or_else(|| CompileError::UnknownVariable(var_name.to_string()))?;
                    dependencies.push(dep_slot);
                    // Add placeholder for this dependency
                    template_parts.push(format!("{{{}}}", dependencies.len() - 1));
                }
            }
        }

        let template = template_parts.join("");
        let slot = self.event_loop.arena.alloc();

        if let Some(node) = self.event_loop.arena.get_mut(slot) {
            node.set_kind(NodeKind::TextTemplate {
                template,
                dependencies: dependencies.clone(),
                cached: None,
            });
        }

        // Subscribe to all dependencies - each gets a distinct input port
        // so we can identify which dependency changed
        for (i, dep) in dependencies.iter().enumerate() {
            self.event_loop.routing.add_route(*dep, slot, Port::Input(i as u8));
        }

        Ok(slot)
    }
    ```

- [ ] **9.12.3** Implement reactive re-rendering on dependency change
  - Content:
    ```rust
    fn process_text_template(&mut self, slot: SlotId) {
        let node = self.arena.get_mut(slot);
        if let Some(NodeKind::TextTemplate { template, dependencies, cached }) = node.kind_mut() {
            // Collect current values from all dependencies
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
            let text: Arc<str> = result.into();
            *cached = Some(text.clone());
            self.emit_to_subscribers(slot, Payload::Text(text));
        }
    }
    ```

- [ ] **9.12.4** Handle nested interpolations
  - Nested expressions like `{item.name}` compile to field access chain
  - Each intermediate SlotId is NOT a dependency - only the final result is

- [ ] **9.12.5** Test: TEXT { Count: {count} } updates when count changes
  - Verify initial render
  - Verify re-render when `count` changes
  - Verify only one dependency tracked (the count slot)

#### 9.13 Full Validation

- [ ] **9.13.1** Test shopping_list.bn (PASS/PASSED + List ops)
- [ ] **9.13.2** Test todo_mvc.bn (FULL VALIDATION TARGET)
- [ ] **9.13.3** Run all 23 example files, note failures
- [ ] **9.13.4** Commit Phase 7d

---

### Phase 7 Summary

| Sub-phase | Duration | Key Deliverables |
|-----------|----------|------------------|
| 7a | 1 week | Basic bridge, scalar rendering, feature flag |
| 7b | 1 week | Element/*, styling, layout |
| 7c | 2 weeks | LINK, events, element states, router |
| 7d | 1 week | PASS/PASSED, TextTemplate, full validation |
| **Total** | **5 weeks** | **todo_mvc.bn works** |

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
  jj new -m "M0 + M1 complete: New arena-based engine + CLI"
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
| Message passing, routing | `docs/new_boon/2.3_MESSAGE_PASSING.md` |
| Event loop | `docs/new_boon/2.4_EVENT_LOOP.md` |
| Error handling (FLUSH) | `docs/new_boon/2.6_ERROR_HANDLING.md` |
| Bridge API | `docs/new_boon/2.8_BRIDGE_API.md` |
| Context passing (PASS/PASSED) | `docs/new_boon/2.11_CONTEXT_PASSING.md` |
| Issues | `docs/new_boon/6.1_ISSUES.md` |
| Examples | `docs/new_boon/6.2_EXAMPLES.md` |
| Canonical plan | `docs/new_boon/plans/M0_CLI__M1_NEW_ENGINE_IN_PLAYGROUND.md` |

---

**Last Updated:** Created for M0/M1 implementation
