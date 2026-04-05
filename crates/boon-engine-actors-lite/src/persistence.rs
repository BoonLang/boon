//! Persistence integration for the ActorsLite runtime.
//!
//! This module connects the `PersistenceAdapter` to the `IrExecutor`.
//! It handles:
//! 1. Restoring persisted HOLD cell values before execution begins
//! 2. Collecting dirty HOLD cells after quiescence
//! 3. Committing the persistence batch to the adapter

use crate::ir::{IrProgram, NodeId, PersistKind, PersistPolicy, SinkPortId};
use crate::ir_executor::IrExecutor;
use crate::persist::{
    InMemoryPersistence, PersistManifest, PersistedRecord, PersistenceAdapter,
    persistence_root_prefix, persistence_slot_key,
};
use boon::parser::PersistenceId;
use boon::platform::browser::kernel::KernelValue;
use std::collections::BTreeMap;

/// Tracks which durable nodes changed during a round of execution.
#[derive(Debug, Default)]
pub struct PersistenceTracker {
    /// Dirty HOLD cells: (root_key, local_slot) → new value.
    dirty_holds: BTreeMap<(String, u32), KernelValue>,
    /// Live durable root keys for GC.
    live_roots: BTreeMap<String, PersistenceId>,
}

impl PersistenceTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark a HOLD cell as dirty with its new value.
    pub fn mark_hold_dirty(&mut self, root_key: String, local_slot: u32, value: KernelValue) {
        self.dirty_holds.insert((root_key, local_slot), value);
    }

    /// Register a live durable root for GC tracking.
    pub fn register_live_root(&mut self, root_key: String, persistence_id: PersistenceId) {
        self.live_roots.insert(root_key.clone(), persistence_id);
    }

    /// Collect the persistence batch to write.
    pub fn collect_writes(&self) -> Vec<PersistedRecord> {
        self.dirty_holds
            .iter()
            .map(|((root_key, local_slot), value)| PersistedRecord::Hold {
                root_key: root_key.clone(),
                local_slot: *local_slot,
                value: kernel_value_to_json(value),
            })
            .collect()
    }

    /// Collect keys to delete (live roots not matching current program).
    pub fn collect_deletes(
        &self,
        existing_records: &[PersistedRecord],
    ) -> Vec<String> {
        let mut delete_keys = Vec::new();
        for record in existing_records {
            let record_key = match record {
                PersistedRecord::Hold { root_key, local_slot, .. } => {
                    format!("{root_key}.{local_slot}")
                }
                PersistedRecord::ListStore { root_key, local_slot, .. } => {
                    format!("{root_key}.{local_slot}")
                }
            };
            // If this key isn't in our live roots, mark for deletion
            let is_live = self
                .live_roots
                .iter()
                .any(|(rk, _pid)| record_key.starts_with(rk.as_str()));
            if !is_live {
                delete_keys.push(record_key);
            }
        }
        delete_keys
    }

    /// Clear dirty markers after successful commit.
    pub fn clear_dirty(&mut self) {
        self.dirty_holds.clear();
    }

    /// Get the live root keys for manifest update.
    pub fn live_root_keys(&self) -> Vec<String> {
        self.live_roots.keys().cloned().collect()
    }
}

/// Convert a KernelValue to JSON for persistence storage.
pub(crate) fn kernel_value_to_json(value: &KernelValue) -> serde_json::Value {
    match value {
        KernelValue::Number(n) => serde_json::json!({ "Number": n }),
        KernelValue::Text(t) => serde_json::json!({ "Text": t }),
        KernelValue::Tag(t) => serde_json::json!({ "Tag": t }),
        KernelValue::Bool(b) => serde_json::json!({ "Bool": b }),
        KernelValue::List(items) => {
            let json_items: Vec<serde_json::Value> = items.iter().map(|item| kernel_value_to_json(item)).collect();
            serde_json::json!({ "List": json_items })
        }
        KernelValue::Object(map) => {
            let json_obj: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .map(|(k, v)| (k.clone(), kernel_value_to_json(v)))
                .collect();
            serde_json::json!({ "Object": json_obj })
        }
        KernelValue::Skip => serde_json::json!({ "Skip": true }),
    }
}

/// Restore a JSON value back to a KernelValue.
pub(crate) fn json_to_kernel_value(value: &serde_json::Value) -> KernelValue {
    if let Some(obj) = value.as_object() {
        if obj.contains_key("Number") {
            if let Some(n) = obj.get("Number").and_then(|v| v.as_f64()) {
                return KernelValue::Number(n);
            }
        }
        if obj.contains_key("Text") {
            if let Some(t) = obj.get("Text").and_then(|v| v.as_str()) {
                return KernelValue::Text(t.to_string());
            }
        }
        if obj.contains_key("Tag") {
            if let Some(t) = obj.get("Tag").and_then(|v| v.as_str()) {
                return KernelValue::Tag(t.to_string());
            }
        }
        if obj.contains_key("Bool") {
            if let Some(b) = obj.get("Bool").and_then(|v| v.as_bool()) {
                return KernelValue::Bool(b);
            }
        }
        if obj.contains_key("List") {
            if let Some(arr) = obj.get("List").and_then(|v| v.as_array()) {
                let items: Vec<KernelValue> = arr.iter().map(|v| json_to_kernel_value(v)).collect();
                return KernelValue::List(items);
            }
        }
        if obj.contains_key("Object") {
            if let Some(inner_obj) = obj.get("Object").and_then(|v| v.as_object()) {
                let map: std::collections::BTreeMap<String, KernelValue> = inner_obj
                    .iter()
                    .map(|(k, v)| (k.clone(), json_to_kernel_value(v)))
                    .collect();
                return KernelValue::Object(map);
            }
        }
    }
    KernelValue::Skip
}

/// Load persisted HOLD values and return a map of (root_key, local_slot) → KernelValue.
pub fn load_persisted_holds(
    adapter: &dyn PersistenceAdapter,
) -> Result<BTreeMap<(String, u32), KernelValue>, String> {
    let records = adapter.load_records()?;
    let mut holds = BTreeMap::new();

    for record in records {
        if let PersistedRecord::Hold {
            root_key,
            local_slot,
            value,
        } = record
        {
            holds.insert(
                (root_key.clone(), local_slot),
                json_to_kernel_value(&value),
            );
        }
    }

    Ok(holds)
}

/// Build a persistence tracker from the IR program's persistence metadata.
pub fn build_persistence_tracker(
    program: &IrProgram,
    persisted_holds: &BTreeMap<(String, u32), KernelValue>,
) -> PersistenceTracker {
    let mut tracker = PersistenceTracker::new();

    for entry in &program.persistence {
        if let PersistPolicy::Durable {
            root_key,
            local_slot,
            persist_kind,
        } = entry.policy
        {
            let root_key_str = root_key.to_string();
            tracker.register_live_root(root_key_str.clone(), root_key);

            if matches!(persist_kind, PersistKind::Hold) {
                // Restore persisted value if available
                if let Some(value) = persisted_holds.get(&(root_key_str.clone(), local_slot)) {
                    // The restore happens at the executor level; here we just track it's live
                    let _ = value;
                }
            }
        }
    }

    tracker
}

/// Default in-memory persistence (no-op for non-browser environments).
pub fn default_persistence_adapter() -> InMemoryPersistence {
    InMemoryPersistence::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{IrNode, IrNodeKind, IrNodePersistence};
    use crate::persist::InMemoryPersistence;

    #[test]
    fn persistence_tracker_tracks_dirty_holds() {
        let mut tracker = PersistenceTracker::new();
        tracker.mark_hold_dirty("test.root".to_string(), 0, KernelValue::Number(42.0));

        let writes = tracker.collect_writes();
        assert_eq!(writes.len(), 1);
        match &writes[0] {
            PersistedRecord::Hold { root_key, local_slot, value } => {
                assert_eq!(root_key, "test.root");
                assert_eq!(*local_slot, 0);
                assert_eq!(value.get("Number").and_then(|v| v.as_f64()), Some(42.0));
            }
            _ => panic!("expected Hold record"),
        }
    }

    #[test]
    fn persistence_roundtrip_in_memory() {
        let adapter = InMemoryPersistence::new();
        let records = adapter.load_records().expect("should load empty");
        assert!(records.is_empty());

        let writes = vec![PersistedRecord::Hold {
            root_key: "test.root".to_string(),
            local_slot: 0,
            value: serde_json::json!({ "Number": 99.0 }),
        }];
        adapter
            .apply_batch(&writes, &[])
            .expect("should apply batch");

        let records = adapter.load_records().expect("should load records");
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn kernel_value_json_roundtrip() {
        let tests = vec![
            KernelValue::Number(3.14),
            KernelValue::Text("hello".to_string()),
            KernelValue::Tag("True".to_string()),
            KernelValue::Bool(true),
        ];

        for val in tests {
            let json = kernel_value_to_json(&val);
            let restored = json_to_kernel_value(&json);
            assert_eq!(val, restored, "roundtrip failed for {val:?}");
        }
    }
}
