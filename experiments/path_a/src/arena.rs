//! Arena-based slot allocator for Path A engine.
//!
//! Slots are pre-allocated positions that hold node state.
//! This is a simplified version focused on the prototype.

use crate::node::Node;
use shared::test_harness::Value;
use std::cell::Cell;
use std::collections::HashSet;

/// Unique identifier for a slot in the arena
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SlotId(pub u32);

impl SlotId {
    pub fn new(id: u32) -> Self {
        Self(id)
    }

    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// Arena that holds all node slots
pub struct Arena {
    /// The nodes in the arena
    nodes: Vec<Option<Node>>,
    /// Current values for each slot
    values: Vec<Value>,
    /// Whether each slot is dirty (needs recomputation)
    /// Uses Cell for interior mutability - allows marking dirty while iterating subscribers
    dirty: Vec<Cell<bool>>,
    /// Subscribers for each slot (slots that depend on this slot)
    /// Uses HashSet for O(1) duplicate checking in add_subscriber
    subscribers: Vec<HashSet<SlotId>>,
    /// Dependencies for each slot (slots this slot depends on)
    /// Used for topological sorting
    dependencies: Vec<HashSet<SlotId>>,
    /// Topological index for each slot (lower = should be evaluated first)
    /// Used for sorting dirty queue in correct order
    topo_index: Vec<u32>,
    /// Next slot ID to allocate
    next_id: u32,
}

impl Arena {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            values: Vec::new(),
            dirty: Vec::new(),
            subscribers: Vec::new(),
            dependencies: Vec::new(),
            topo_index: Vec::new(),
            next_id: 0,
        }
    }

    /// Allocate a new slot and return its ID
    pub fn alloc(&mut self) -> SlotId {
        let id = SlotId(self.next_id);
        self.next_id += 1;
        self.nodes.push(None);
        self.values.push(Value::Skip);
        self.dirty.push(Cell::new(false));
        self.subscribers.push(HashSet::new());
        self.dependencies.push(HashSet::new());
        self.topo_index.push(u32::MAX); // Will be set by compute_topo_order
        id
    }

    /// Get topological index for a slot
    pub fn get_topo_index(&self, slot: SlotId) -> u32 {
        self.topo_index.get(slot.index()).copied().unwrap_or(u32::MAX)
    }

    /// Set topological index for a slot
    pub fn set_topo_index(&mut self, slot: SlotId, index: u32) {
        if slot.index() < self.topo_index.len() {
            self.topo_index[slot.index()] = index;
        }
    }

    /// Set the node for a slot
    pub fn set_node(&mut self, slot: SlotId, node: Node) {
        let idx = slot.index();
        if idx < self.nodes.len() {
            self.nodes[idx] = Some(node);
        }
    }

    /// Get the node for a slot
    pub fn get_node(&self, slot: SlotId) -> Option<&Node> {
        self.nodes.get(slot.index())?.as_ref()
    }

    /// Get mutable node for a slot
    pub fn get_node_mut(&mut self, slot: SlotId) -> Option<&mut Node> {
        self.nodes.get_mut(slot.index())?.as_mut()
    }

    /// Set the value for a slot
    pub fn set_value(&mut self, slot: SlotId, value: Value) {
        let idx = slot.index();
        if idx < self.values.len() {
            self.values[idx] = value;
        }
    }

    /// Get the value for a slot
    pub fn get_value(&self, slot: SlotId) -> &Value {
        self.values.get(slot.index()).unwrap_or(&Value::Skip)
    }

    /// Mark a slot as dirty
    /// Uses interior mutability (Cell) to allow marking dirty while iterating subscribers
    pub fn mark_dirty(&self, slot: SlotId) {
        let idx = slot.index();
        if idx < self.dirty.len() {
            self.dirty[idx].set(true);
        }
    }

    /// Check if a slot is dirty
    pub fn is_dirty(&self, slot: SlotId) -> bool {
        self.dirty.get(slot.index()).map(|c| c.get()).unwrap_or(false)
    }

    /// Clear dirty flag for a slot
    pub fn clear_dirty(&self, slot: SlotId) {
        let idx = slot.index();
        if idx < self.dirty.len() {
            self.dirty[idx].set(false);
        }
    }

    /// Add a subscriber to a slot
    /// O(1) with HashSet instead of O(n) with Vec
    pub fn add_subscriber(&mut self, source: SlotId, subscriber: SlotId) {
        let idx = source.index();
        if idx < self.subscribers.len() {
            self.subscribers[idx].insert(subscriber);
        }
    }

    /// Get subscribers for a slot
    /// Returns an iterator over the HashSet
    pub fn get_subscribers(&self, slot: SlotId) -> impl Iterator<Item = &SlotId> {
        self.subscribers
            .get(slot.index())
            .into_iter()
            .flat_map(|s| s.iter())
    }

    /// Add a dependency to a slot (for topological sorting)
    pub fn add_dependency(&mut self, slot: SlotId, dependency: SlotId) {
        let idx = slot.index();
        if idx < self.dependencies.len() {
            self.dependencies[idx].insert(dependency);
        }
    }

    /// Get dependencies for a slot
    pub fn get_dependencies(&self, slot: SlotId) -> impl Iterator<Item = &SlotId> {
        self.dependencies
            .get(slot.index())
            .into_iter()
            .flat_map(|s| s.iter())
    }

    /// Assign incremental topo-index for a newly created slot
    /// Sets topo_index = 1 + max(topo_index of dependencies)
    /// This ensures new slots process after their dependencies without full recompute
    pub fn assign_incremental_topo_index(&mut self, slot: SlotId) {
        let max_dep_index = self.get_dependencies(slot)
            .map(|dep| self.get_topo_index(*dep))
            .filter(|&idx| idx != u32::MAX)
            .max()
            .unwrap_or(0);

        // Set topo_index slightly after dependencies
        // Using +1 ensures this slot processes after its deps
        self.set_topo_index(slot, max_dep_index.saturating_add(1));
    }

    /// Assign incremental topo-indices for a range of new slots
    /// Processes slots in index order, assigning proper indices based on dependencies
    pub fn assign_incremental_topo_indices(&mut self, start_index: usize) {
        for i in start_index..self.len() {
            self.assign_incremental_topo_index(SlotId(i as u32));
        }
    }

    /// Compute topological order of all slots using Kahn's algorithm
    /// Returns slots ordered so that dependencies come before dependents
    /// Also updates topo_index for each slot for O(1) lookup
    pub fn compute_topo_order(&mut self) -> Vec<SlotId> {
        let n = self.len();
        if n == 0 {
            return Vec::new();
        }

        // Count in-degrees (number of dependencies)
        let mut in_degree: Vec<usize> = vec![0; n];
        for idx in 0..n {
            in_degree[idx] = self.dependencies.get(idx).map(|d| d.len()).unwrap_or(0);
        }

        // Start with slots that have no dependencies
        let mut queue: std::collections::VecDeque<SlotId> = std::collections::VecDeque::new();
        for idx in 0..n {
            if in_degree[idx] == 0 {
                queue.push_back(SlotId(idx as u32));
            }
        }

        let mut result = Vec::with_capacity(n);

        while let Some(slot) = queue.pop_front() {
            // Set topological index
            self.topo_index[slot.index()] = result.len() as u32;
            result.push(slot);

            // For each subscriber (slot that depends on us), decrease in-degree
            let subs: Vec<SlotId> = self.subscribers.get(slot.index())
                .map(|s| s.iter().copied().collect())
                .unwrap_or_default();
            for sub in subs {
                let sub_idx = sub.index();
                if sub_idx < in_degree.len() {
                    in_degree[sub_idx] = in_degree[sub_idx].saturating_sub(1);
                    if in_degree[sub_idx] == 0 {
                        queue.push_back(sub);
                    }
                }
            }
        }

        // If we couldn't order all slots, there might be cycles
        // Add remaining slots in index order as fallback
        if result.len() < n {
            for idx in 0..n {
                let slot = SlotId(idx as u32);
                if self.topo_index[idx] == u32::MAX {
                    self.topo_index[idx] = result.len() as u32;
                    result.push(slot);
                }
            }
        }

        result
    }

    /// Get all dirty slots as an iterator (avoids Vec allocation)
    pub fn dirty_slots(&self) -> impl Iterator<Item = SlotId> + '_ {
        self.dirty
            .iter()
            .enumerate()
            .filter_map(|(idx, dirty)| {
                if dirty.get() {
                    Some(SlotId(idx as u32))
                } else {
                    None
                }
            })
    }

    /// Number of allocated slots
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Check if arena is empty
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

impl Default for Arena {
    fn default() -> Self {
        Self::new()
    }
}
