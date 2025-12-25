use std::collections::HashMap;
use std::sync::Arc;
use super::address::NodeAddress;
use super::message::Payload;
use super::node::NodeKind;

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

    /// Get field ID from name (if interned).
    pub fn get_field_id(&self, name: &str) -> Option<u32> {
        self.field_ids.get(name).copied()
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

    /// Free a slot, making it available for reuse.
    pub fn free(&mut self, slot: SlotId) {
        if self.is_valid(slot) {
            // Bump generation immediately to invalidate the slot
            self.nodes[slot.index as usize].generation += 1;
            self.free_list.push(slot.index);
            self.addresses.remove(&slot);
        }
    }

    /// Check if a SlotId is valid (correct generation).
    pub fn is_valid(&self, slot: SlotId) -> bool {
        slot.index < self.nodes.len() as u32
            && self.nodes[slot.index as usize].generation == slot.generation
    }

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

    /// Get the number of slots in the arena (including freed slots).
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Check if the arena is empty.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Iterate over field names for snapshot serialization.
    pub fn iter_field_names(&self) -> impl Iterator<Item = (u32, &Arc<str>)> {
        self.field_names.iter().map(|(&id, name)| (id, name))
    }

    /// Iterate over tag names for snapshot serialization.
    pub fn iter_tag_names(&self) -> impl Iterator<Item = (u32, &Arc<str>)> {
        self.tag_names.iter().map(|(&id, name)| (id, name))
    }
}

impl Default for Arena {
    fn default() -> Self {
        Self::new()
    }
}

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
