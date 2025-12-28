//! Delta ledger for Path A engine.
//!
//! Records all state changes for debugging and time-travel.

use crate::arena::SlotId;
use shared::test_harness::Value;

/// A recorded delta entry
#[derive(Debug, Clone)]
pub struct DeltaEntry {
    /// The tick when this happened
    pub tick: u64,
    /// The kind of change
    pub kind: DeltaKind,
}

/// Different kinds of deltas
#[derive(Debug, Clone)]
pub enum DeltaKind {
    /// A slot value was set
    Set {
        slot: SlotId,
        old_value: Value,
        new_value: Value,
    },
    /// An item was inserted into a list
    ListInsert {
        list_slot: SlotId,
        index: usize,
        item: Value,
    },
    /// An item was removed from a list
    ListRemove {
        list_slot: SlotId,
        index: usize,
        item: Value,
    },
    /// An event was injected
    Event {
        path: String,
        payload: Value,
    },
    /// A LINK was bound
    LinkBind {
        link_slot: SlotId,
        target_slot: SlotId,
    },
}

/// The delta ledger
#[derive(Default)]
pub struct Ledger {
    entries: Vec<DeltaEntry>,
    enabled: bool,
}

impl Ledger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable ledger recording
    pub fn enable(&mut self) {
        self.enabled = true;
    }

    /// Disable ledger recording
    pub fn disable(&mut self) {
        self.enabled = false;
    }

    /// Record a delta
    pub fn record(&mut self, tick: u64, kind: DeltaKind) {
        if self.enabled {
            self.entries.push(DeltaEntry { tick, kind });
        }
    }

    /// Get all entries
    pub fn entries(&self) -> &[DeltaEntry] {
        &self.entries
    }

    /// Clear all entries
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Get entries for a specific tick
    pub fn entries_for_tick(&self, tick: u64) -> Vec<&DeltaEntry> {
        self.entries.iter().filter(|e| e.tick == tick).collect()
    }
}
