//! Cell types for Path B engine.
//!
//! Cells store persistent state that survives across ticks.

use crate::slot::SlotKey;
use shared::test_harness::Value;

/// Cell for HOLD state
#[derive(Debug, Clone)]
pub struct HoldCell {
    /// Current held value
    pub value: Value,
}

impl HoldCell {
    pub fn new(initial: Value) -> Self {
        Self { value: initial }
    }
}

/// Cell for LINK bindings
#[derive(Debug, Clone)]
pub struct LinkCell {
    /// Bound target slot (if any)
    pub bound: Option<SlotKey>,
    /// Pending event value
    pub pending_event: Option<Value>,
}

impl LinkCell {
    pub fn new() -> Self {
        Self {
            bound: None,
            pending_event: None,
        }
    }

    pub fn bind(&mut self, target: SlotKey) {
        self.bound = Some(target);
    }

    pub fn inject(&mut self, value: Value) {
        self.pending_event = Some(value);
    }

    pub fn take_event(&mut self) -> Option<Value> {
        self.pending_event.take()
    }
}

impl Default for LinkCell {
    fn default() -> Self {
        Self::new()
    }
}

/// Item key for list elements
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ItemKey(pub u64);

/// Cell for LIST state
#[derive(Debug, Clone)]
pub struct ListCell {
    /// Item keys in order
    pub keys: Vec<ItemKey>,
    /// Next key to allocate
    next_key: u64,
}

impl ListCell {
    pub fn new() -> Self {
        Self {
            keys: Vec::new(),
            next_key: 0,
        }
    }

    /// Allocate a new item key
    pub fn alloc_key(&mut self) -> ItemKey {
        let key = ItemKey(self.next_key);
        self.next_key += 1;
        key
    }

    /// Append a new item
    pub fn append(&mut self) -> ItemKey {
        let key = self.alloc_key();
        self.keys.push(key);
        key
    }

    /// Remove an item by key
    pub fn remove(&mut self, key: ItemKey) -> bool {
        if let Some(pos) = self.keys.iter().position(|k| *k == key) {
            self.keys.remove(pos);
            true
        } else {
            false
        }
    }

    /// Get number of items
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Get keys iterator
    pub fn iter(&self) -> impl Iterator<Item = &ItemKey> {
        self.keys.iter()
    }
}

impl Default for ListCell {
    fn default() -> Self {
        Self::new()
    }
}
