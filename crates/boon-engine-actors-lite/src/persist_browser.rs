//! Browser localStorage persistence adapter.
//!
//! Used in wasm32 builds to persist durable HOLD cells and list stores
//! to the browser's localStorage API.

use crate::persist::{
    PersistManifest, PersistedRecord, PersistenceAdapter, ENGINE_PREFIX,
};

/// Get the localStorage instance, or return an error.
fn storage() -> Result<boon::zoon::web_sys::Storage, String> {
    boon::zoon::web_sys::window()
        .ok_or_else(|| "no window".to_string())?
        .local_storage()
        .map_err(|e| format!("localStorage unavailable: {e:?}"))?
        .ok_or_else(|| "localStorage is None".to_string())
}

/// Browser localStorage implementation of PersistenceAdapter.
pub struct BrowserLocalStorage;

impl BrowserLocalStorage {
    /// Get a static reference to the BrowserLocalStorage adapter.
    /// This is safe because BrowserLocalStorage is a zero-sized type.
    pub fn instance() -> &'static dyn PersistenceAdapter {
        static ADAPTER: BrowserLocalStorage = BrowserLocalStorage;
        &ADAPTER
    }

    fn manifest_key() -> String {
        format!("{ENGINE_PREFIX}._manifest")
    }

    fn record_key(root_key: &str, local_slot: u32, record_type: &str) -> String {
        format!("{ENGINE_PREFIX}.{root_key}.{local_slot}.{record_type}")
    }
}

impl PersistenceAdapter for BrowserLocalStorage {
    fn load_manifest(&self) -> Result<PersistManifest, String> {
        let storage = storage()?;
        let key = Self::manifest_key();
        match storage.get_item(&key) {
            Ok(Some(text)) => {
                serde_json::from_str(&text).map_err(|e| format!("manifest parse: {e}"))
            }
            Ok(None) => Ok(PersistManifest::default()),
            Err(e) => Err(format!("manifest read: {e:?}")),
        }
    }

    fn save_manifest(&self, manifest: &PersistManifest) -> Result<(), String> {
        let storage = storage()?;
        let key = Self::manifest_key();
        let text = serde_json::to_string(manifest).map_err(|e| e.to_string())?;
        storage
            .set_item(&key, &text)
            .map_err(|e| format!("manifest write: {e:?}"))
    }

    fn load_records(&self) -> Result<Vec<PersistedRecord>, String> {
        let storage = storage()?;
        let prefix = format!("{ENGINE_PREFIX}.");
        let mut records = Vec::new();

        let len = storage.length().map_err(|e| format!("length error: {e:?}"))?;
        for i in 0..len {
            let key = match storage.key(i) {
                Ok(Some(k)) => k,
                _ => continue,
            };

            if !key.starts_with(&prefix) || key.ends_with("_manifest") {
                continue;
            }

            let value = match storage.get_item(&key) {
                Ok(Some(v)) => v,
                _ => continue,
            };

            if let Ok(record) = serde_json::from_str::<PersistedRecord>(&value) {
                records.push(record);
            }
        }

        Ok(records)
    }

    fn apply_batch(
        &self,
        writes: &[PersistedRecord],
        delete_keys: &[String],
    ) -> Result<(), String> {
        let storage = storage()?;

        // Delete stale keys
        for key in delete_keys {
            let _ = storage.remove_item(key);
        }

        // Write new/updated records
        for record in writes {
            let (key, value) = match record {
                PersistedRecord::Hold {
                    root_key,
                    local_slot,
                    ..
                } => (
                    Self::record_key(root_key, *local_slot, "hold"),
                    serde_json::to_string(record).map_err(|e| e.to_string())?,
                ),
                PersistedRecord::ListStore {
                    root_key,
                    local_slot,
                    ..
                } => (
                    Self::record_key(root_key, *local_slot, "list"),
                    serde_json::to_string(record).map_err(|e| e.to_string())?,
                ),
            };
            storage
                .set_item(&key, &value)
                .map_err(|e| format!("write error: {e:?}"))?;
        }

        Ok(())
    }
}
