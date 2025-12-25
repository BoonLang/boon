//! Snapshot system for state persistence.
//!
//! Serializes and deserializes the reactive graph state for persistence.
//!
//! Note: JSON serialization requires the `cli` feature.

use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use super::arena::Arena;
use super::message::Payload;
use super::address::{SourceId, ScopeId};

/// A serializable snapshot of the reactive graph state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphSnapshot {
    /// Version for migration support
    pub version: u32,
    /// Persisted node values: "source_id:scope_id" -> serialized Payload
    /// Using string keys because JSON requires string keys in maps.
    pub values: HashMap<String, SerializedPayload>,
    /// Field name intern table
    pub field_names: HashMap<String, String>,
    /// Tag name intern table
    pub tag_names: HashMap<String, String>,
}

/// A serializable representation of a Payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SerializedPayload {
    Number(f64),
    Text(String),
    Bool(bool),
    Unit,
    Tag(u32),
    List(Vec<SerializedPayload>),
    Object(HashMap<String, SerializedPayload>),
    TaggedObject { tag: u32, fields: HashMap<String, SerializedPayload> },
}

impl GraphSnapshot {
    /// Current snapshot version.
    pub const VERSION: u32 = 1;

    /// Create a new empty snapshot.
    pub fn new() -> Self {
        Self {
            version: Self::VERSION,
            values: HashMap::new(),
            field_names: HashMap::new(),
            tag_names: HashMap::new(),
        }
    }

    /// Serialize a Payload to SerializedPayload.
    pub fn serialize_payload(payload: &Payload, arena: &Arena) -> SerializedPayload {
        match payload {
            Payload::Number(n) => SerializedPayload::Number(*n),
            Payload::Text(s) => SerializedPayload::Text(s.to_string()),
            Payload::Bool(b) => SerializedPayload::Bool(*b),
            Payload::Unit => SerializedPayload::Unit,
            Payload::Tag(t) => SerializedPayload::Tag(*t),
            // Handle lists and objects by collecting their items
            Payload::ListHandle(_slot) => {
                // Would traverse list items - simplified for now
                SerializedPayload::List(Vec::new())
            }
            Payload::ObjectHandle(_slot) => {
                // Would traverse object fields - simplified for now
                SerializedPayload::Object(HashMap::new())
            }
            Payload::TaggedObject { tag, fields: _ } => {
                SerializedPayload::TaggedObject {
                    tag: *tag,
                    fields: HashMap::new(),
                }
            }
            Payload::Flushed(inner) => {
                // Don't persist errors
                SerializedPayload::Unit
            }
            Payload::ListDelta(_) | Payload::ObjectDelta(_) => {
                // Deltas are transient, don't persist
                SerializedPayload::Unit
            }
        }
    }

    /// Store a value in the snapshot.
    pub fn store(&mut self, source_id: SourceId, scope_id: ScopeId, payload: &Payload, arena: &Arena) {
        let key = format!("{}:{}", source_id.stable_id, scope_id.0);
        let value = Self::serialize_payload(payload, arena);
        self.values.insert(key, value);
    }

    /// Retrieve a value from the snapshot.
    pub fn retrieve(&self, source_id: SourceId, scope_id: ScopeId) -> Option<&SerializedPayload> {
        let key = format!("{}:{}", source_id.stable_id, scope_id.0);
        self.values.get(&key)
    }

    /// Copy intern tables from arena.
    pub fn copy_intern_tables(&mut self, arena: &Arena) {
        // Field names
        for (id, name) in arena.iter_field_names() {
            self.field_names.insert(id.to_string(), name.to_string());
        }
        // Tag names
        for (id, name) in arena.iter_tag_names() {
            self.tag_names.insert(id.to_string(), name.to_string());
        }
    }

    /// Serialize snapshot to JSON string.
    #[cfg(feature = "cli")]
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Deserialize snapshot from JSON string.
    #[cfg(feature = "cli")]
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

impl Default for GraphSnapshot {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_store_retrieve() {
        let mut snapshot = GraphSnapshot::new();

        let source_id = SourceId { stable_id: 42, parse_order: 1 };
        let scope_id = ScopeId(100);
        let payload = Payload::Number(123.0);
        let arena = Arena::new();

        snapshot.store(source_id, scope_id, &payload, &arena);

        let value = snapshot.retrieve(source_id, scope_id).unwrap();
        match value {
            SerializedPayload::Number(n) => assert_eq!(*n, 123.0),
            _ => panic!("Expected Number"),
        }
    }
}
