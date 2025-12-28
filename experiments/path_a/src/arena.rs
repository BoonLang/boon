//! Arena-based slot allocator for Path A engine.
//!
//! Slots are pre-allocated positions that hold node state.
//! This is a simplified version focused on the prototype.

use crate::node::Node;
use shared::test_harness::Value;

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
    dirty: Vec<bool>,
    /// Subscribers for each slot (slots that depend on this slot)
    subscribers: Vec<Vec<SlotId>>,
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
            next_id: 0,
        }
    }

    /// Allocate a new slot and return its ID
    pub fn alloc(&mut self) -> SlotId {
        let id = SlotId(self.next_id);
        self.next_id += 1;
        self.nodes.push(None);
        self.values.push(Value::Skip);
        self.dirty.push(false);
        self.subscribers.push(Vec::new());
        id
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
    pub fn mark_dirty(&mut self, slot: SlotId) {
        let idx = slot.index();
        if idx < self.dirty.len() {
            self.dirty[idx] = true;
        }
    }

    /// Check if a slot is dirty
    pub fn is_dirty(&self, slot: SlotId) -> bool {
        self.dirty.get(slot.index()).copied().unwrap_or(false)
    }

    /// Clear dirty flag for a slot
    pub fn clear_dirty(&mut self, slot: SlotId) {
        let idx = slot.index();
        if idx < self.dirty.len() {
            self.dirty[idx] = false;
        }
    }

    /// Add a subscriber to a slot
    pub fn add_subscriber(&mut self, source: SlotId, subscriber: SlotId) {
        let idx = source.index();
        if idx < self.subscribers.len() {
            if !self.subscribers[idx].contains(&subscriber) {
                self.subscribers[idx].push(subscriber);
            }
        }
    }

    /// Get subscribers for a slot
    pub fn get_subscribers(&self, slot: SlotId) -> &[SlotId] {
        self.subscribers
            .get(slot.index())
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Get all dirty slots
    pub fn dirty_slots(&self) -> Vec<SlotId> {
        self.dirty
            .iter()
            .enumerate()
            .filter_map(|(idx, &dirty)| {
                if dirty {
                    Some(SlotId(idx as u32))
                } else {
                    None
                }
            })
            .collect()
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
