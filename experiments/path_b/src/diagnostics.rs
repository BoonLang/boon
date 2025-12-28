//! Diagnostics for Path B engine.
//!
//! Provides "why did X change?" queries for debugging.

use crate::cache::Cache;
use crate::slot::SlotKey;
use crate::tick::TickSeq;

/// A diagnostic query result showing why a value changed
#[derive(Debug, Clone)]
pub struct ChangeReason {
    /// The slot that changed
    pub slot: SlotKey,
    /// When it changed
    pub changed_at: TickSeq,
    /// Direct dependencies that triggered the change
    pub triggered_by: Vec<SlotKey>,
}

/// Query why a value changed
pub fn why_did_change(cache: &Cache, slot: &SlotKey, tick: u64) -> Option<ChangeReason> {
    let entry = cache.get(slot)?;

    if entry.last_changed.tick != tick {
        // Didn't change this tick
        return None;
    }

    // Find which dependencies changed this tick
    let triggered_by: Vec<SlotKey> = entry
        .deps
        .iter()
        .filter(|dep| {
            cache
                .get(dep)
                .map(|e| e.last_changed.tick == tick)
                .unwrap_or(false)
        })
        .cloned()
        .collect();

    Some(ChangeReason {
        slot: slot.clone(),
        changed_at: entry.last_changed,
        triggered_by,
    })
}

/// Get the full change chain for debugging
pub fn change_chain(cache: &Cache, slot: &SlotKey, tick: u64) -> Vec<ChangeReason> {
    let mut chain = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let mut queue = vec![slot.clone()];

    while let Some(current) = queue.pop() {
        if visited.contains(&current) {
            continue;
        }
        visited.insert(current.clone());

        if let Some(reason) = why_did_change(cache, &current, tick) {
            for trigger in &reason.triggered_by {
                queue.push(trigger.clone());
            }
            chain.push(reason);
        }
    }

    chain
}

/// Diagnostics context for the runtime
#[derive(Debug, Default)]
pub struct DiagnosticsContext {
    /// Enable detailed tracking
    pub enabled: bool,
    /// Recorded change events
    pub changes: Vec<ChangeEvent>,
}

/// A recorded change event
#[derive(Debug, Clone)]
pub struct ChangeEvent {
    pub tick: u64,
    pub slot: SlotKey,
    pub old_value: Option<shared::test_harness::Value>,
    pub new_value: shared::test_harness::Value,
}

impl DiagnosticsContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn enable(&mut self) {
        self.enabled = true;
    }

    pub fn record_change(
        &mut self,
        tick: u64,
        slot: SlotKey,
        old_value: Option<shared::test_harness::Value>,
        new_value: shared::test_harness::Value,
    ) {
        if self.enabled {
            self.changes.push(ChangeEvent {
                tick,
                slot,
                old_value,
                new_value,
            });
        }
    }

    pub fn changes_at_tick(&self, tick: u64) -> Vec<&ChangeEvent> {
        self.changes.iter().filter(|e| e.tick == tick).collect()
    }
}
