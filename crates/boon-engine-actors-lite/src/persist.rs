//! Persistence adapter for ActorsLite.
//!
//! Defines the trait and platform-specific implementations for
//! persisting durable runtime state (HOLD cells, list stores).

use boon::parser::PersistenceId;
use serde::{Deserialize, Serialize};

/// Namespaced persistence key prefix.
/// Format: `{ENGINE}.{SCHEMA_VERSION}`
pub(crate) const ENGINE_PREFIX: &str = "boon.actorslite.v1";

/// A persisted record in storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PersistedRecord {
    /// A durable HOLD cell value.
    Hold {
        root_key: String,
        local_slot: u32,
        /// Serialized value (opaque JSON).
        value: serde_json::Value,
    },
    /// A list store's membership and item ids.
    ListStore {
        root_key: String,
        local_slot: u32,
        next_item_id: u64,
        /// Items: (item_id, child_key).
        items: Vec<(u64, String)>,
    },
}

/// Manifest tracking live root keys and generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistManifest {
    pub schema_version: u32,
    pub live_root_keys: Vec<String>,
    pub generation: u64,
}

impl Default for PersistManifest {
    fn default() -> Self {
        Self {
            schema_version: 1,
            live_root_keys: Vec::new(),
            generation: 0,
        }
    }
}

/// Trait for persistence storage backends.
pub trait PersistenceAdapter {
    /// Load the manifest for the current namespace.
    fn load_manifest(&self) -> Result<PersistManifest, String>;

    /// Save the manifest.
    fn save_manifest(&self, manifest: &PersistManifest) -> Result<(), String>;

    /// Load all persisted records.
    fn load_records(&self) -> Result<Vec<PersistedRecord>, String>;

    /// Apply a batch of changes (writes + deletes).
    fn apply_batch(
        &self,
        writes: &[PersistedRecord],
        delete_keys: &[String],
    ) -> Result<(), String>;
}

/// In-memory no-op adapter for testing without persistence.
#[derive(Default)]
pub struct InMemoryPersistence {
    manifest: PersistManifest,
    records: std::sync::Mutex<Vec<PersistedRecord>>,
}

impl InMemoryPersistence {
    pub fn new() -> Self {
        Self::default()
    }
}

impl PersistenceAdapter for InMemoryPersistence {
    fn load_manifest(&self) -> Result<PersistManifest, String> {
        Ok(self.manifest.clone())
    }

    fn save_manifest(&self, manifest: &PersistManifest) -> Result<(), String> {
        // In a real implementation this would update self.manifest,
        // but for testing we just accept it.
        let _ = manifest;
        Ok(())
    }

    fn load_records(&self) -> Result<Vec<PersistedRecord>, String> {
        let guard = self.records.lock().map_err(|e| e.to_string())?;
        Ok(guard.clone())
    }

    fn apply_batch(
        &self,
        writes: &[PersistedRecord],
        delete_keys: &[String],
    ) -> Result<(), String> {
        let mut guard = self.records.lock().map_err(|e| e.to_string())?;

        // Delete stale keys
        for del_key in delete_keys {
            guard.retain(|r| {
                let r_key = match r {
                    PersistedRecord::Hold { root_key, local_slot, .. } => {
                        format!("{root_key}.{local_slot}.hold")
                    }
                    PersistedRecord::ListStore { root_key, local_slot, .. } => {
                        format!("{root_key}.{local_slot}.list")
                    }
                };
                r_key != *del_key
            });
        }

        // Upsert writes
        for write in writes {
            let w_key = match write {
                PersistedRecord::Hold { root_key, local_slot, .. } => {
                    format!("{root_key}.{local_slot}.hold")
                }
                PersistedRecord::ListStore { root_key, local_slot, .. } => {
                    format!("{root_key}.{local_slot}.list")
                }
            };
            guard.retain(|r| {
                let r_key = match r {
                    PersistedRecord::Hold { root_key, local_slot, .. } => {
                        format!("{root_key}.{local_slot}.hold")
                    }
                    PersistedRecord::ListStore { root_key, local_slot, .. } => {
                        format!("{root_key}.{local_slot}.list")
                    }
                };
                r_key != w_key
            });
            guard.push(write.clone());
        }

        Ok(())
    }
}

/// Derive a unique storage key for a durable slot.
pub fn persistence_slot_key(root_key: &PersistenceId, local_slot: u32) -> String {
    format!("{ENGINE_PREFIX}.{root_key:?}.{local_slot}")
}

/// Derive the key prefix for all slots under a durable root.
pub fn persistence_root_prefix(root_key: &PersistenceId) -> String {
    format!("{ENGINE_PREFIX}.{root_key:?}.")
}
