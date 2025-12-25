use std::sync::Arc;
use super::arena::SlotId;
use super::address::NodeAddress;

/// Field identifier - interned string index for O(1) lookup.
///
/// **Engine representation:** `u32` index into global intern table
/// **Protocol JSON:** String field name (human-readable, see §6.8)
/// **Persistence:** Intern table serialized alongside snapshot
///
/// Use `intern_table.get_name(field_id)` to recover the string.
/// Use `intern_table.intern("field_name")` to get the FieldId.
pub type FieldId = u32;

/// Key identifying a list item (from AllocSite).
pub type ItemKey = u64;

/// Delta for efficient list updates.
#[derive(Clone, Debug, PartialEq)]
pub enum ListDelta {
    Insert { key: ItemKey, index: u32, value: Box<Payload> },    // Add item at index
    Update { key: ItemKey, value: Box<Payload> },                 // Replace item value
    FieldUpdate { key: ItemKey, field: FieldId, value: Box<Payload> }, // Nested field within item
    Remove { key: ItemKey },                                 // Remove item by key
    Move { key: ItemKey, from_index: u32, to_index: u32 },   // Reorder item
    Replace { items: Vec<(ItemKey, Box<Payload>)> },              // Full list replacement
}

/// Delta for efficient object updates.
#[derive(Clone, Debug, PartialEq)]
pub enum ObjectDelta {
    FieldUpdate { field: FieldId, value: Box<Payload> },
    FieldRemove { field: FieldId },
}

/// Payload carried by reactive messages.
#[derive(Debug, Clone, PartialEq)]
pub enum Payload {
    Number(f64),
    Text(Arc<str>),
    Tag(u32),
    Bool(bool),
    Unit,
    ListHandle(SlotId),
    ObjectHandle(SlotId),
    TaggedObject { tag: u32, fields: SlotId },
    Flushed(Box<Payload>),
    ListDelta(ListDelta),
    ObjectDelta(ObjectDelta),
}

impl Payload {
    /// Convert payload to display string for text interpolation.
    pub fn to_display_string(&self) -> String {
        match self {
            Payload::Number(n) => n.to_string(),
            Payload::Text(s) => s.to_string(),
            Payload::Bool(b) => b.to_string(),
            Payload::Unit => String::new(),
            Payload::Tag(t) => format!("Tag({})", t),
            Payload::TaggedObject { tag, .. } => format!("TaggedObject({})", tag),
            Payload::ListHandle(_) => "[list]".to_string(),
            Payload::ObjectHandle(_) => "{object}".to_string(),
            Payload::Flushed(inner) => format!("Error: {}", inner.to_display_string()),
            Payload::ListDelta(_) => "[delta]".to_string(),
            Payload::ObjectDelta(_) => "{delta}".to_string(),
        }
    }
}

#[cfg(feature = "cli")]
impl Payload {
    /// Convert payload to JSON for CLI output.
    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::json;
        match self {
            Payload::Number(n) => json!(n),
            Payload::Text(s) => json!(s.as_ref()),
            Payload::Bool(b) => json!(b),
            Payload::Unit => json!(null),
            Payload::Tag(t) => json!(format!("Tag({})", t)),
            Payload::TaggedObject { tag, .. } => json!({"_tag": tag}),
            Payload::ListHandle(_) => json!("[list]"),
            Payload::ObjectHandle(_) => json!("{object}"),
            Payload::Flushed(inner) => json!({"error": inner.to_json()}),
            Payload::ListDelta(_) => json!("[delta]"),
            Payload::ObjectDelta(_) => json!("{delta}"),
        }
    }
}

/// A message sent between reactive nodes.
#[derive(Clone, Debug)]
pub struct Message {
    pub source: NodeAddress,
    pub payload: Payload,
    pub version: u64,
    pub idempotency_key: u64,
}

impl Message {
    pub fn new(source: NodeAddress, payload: Payload) -> Self {
        Self {
            source,
            payload,
            version: 0,
            idempotency_key: 0,
        }
    }
}
