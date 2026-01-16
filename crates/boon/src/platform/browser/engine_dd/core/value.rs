//! DD-compatible value types for Boon.
//!
//! These are simple data types that can flow through DD dataflows.
//! Unlike the actor-based `Value` in engine.rs, these are pure data.

use std::collections::BTreeMap;
use std::sync::Arc;

use ordered_float::OrderedFloat;

use super::types::{CellId, LinkId};

// ═══════════════════════════════════════════════════════════════════════════
// SURGICALLY REMOVED (Phase 6.3): ComputedType enum
// This enum stored "recipes" for imperative computations that bypassed DD.
//
// Phase 7 TODO: Replace with DD operators:
//   - ListCount → DD collection.count() operator
//   - ListCountWhere → DD collection.filter().count() operators
//   - Subtract, GreaterThanZero, Equal → DD arithmetic operators
//   - All computed values should flow through DD dataflow graph
// ═══════════════════════════════════════════════════════════════════════════

// ============================================================================
// DD COLLECTION TYPES
// ============================================================================

use std::sync::atomic::{AtomicU64, Ordering};

/// Global counter for generating unique collection IDs.
static COLLECTION_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Unique identifier for a DD collection in the dataflow graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CollectionId(u64);

impl CollectionId {
    /// Create a new unique collection ID.
    pub fn new() -> Self {
        Self(COLLECTION_ID_COUNTER.fetch_add(1, Ordering::SeqCst))
    }

    /// Get the raw ID value.
    pub fn id(&self) -> u64 {
        self.0
    }
}

impl Default for CollectionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for CollectionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "collection_{}", self.0)
    }
}

/// Handle to a DD collection - provides O(delta) list operations.
///
/// CollectionHandle wraps a DD collection in the dataflow graph.
/// Operations like filter, map, count work incrementally - only
/// processing changes (deltas), not the entire list.
///
/// # Anti-Cheat Design
///
/// CollectionHandle does NOT provide synchronous access to the full list.
/// All reads must go through the DD dataflow system, ensuring reactive
/// updates propagate correctly.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CollectionHandle {
    /// Unique identifier for this collection in the DD graph
    pub id: CollectionId,
    /// The cell ID where this collection's state is stored (for mutations)
    pub cell_id: Option<Arc<str>>,
    /// Snapshot of current items (for rendering - updated reactively)
    /// This is populated by the DD output stream, not read directly
    items: Arc<Vec<Value>>,
}

impl CollectionHandle {
    /// Create a new collection handle with initial items.
    pub fn new(items: Vec<Value>) -> Self {
        Self {
            id: CollectionId::new(),
            cell_id: None,
            items: Arc::new(items),
        }
    }

    /// Create a collection handle linked to a cell for mutations.
    pub fn with_cell(items: Vec<Value>, cell_id: impl Into<String>) -> Self {
        Self {
            id: CollectionId::new(),
            cell_id: Some(Arc::from(cell_id.into())),
            items: Arc::new(items),
        }
    }

    /// Get the current items snapshot (for rendering).
    /// Note: This is the last-known state from DD, not a synchronous read.
    pub fn items(&self) -> &[Value] {
        &self.items
    }

    /// Get the number of items (from snapshot).
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Check if empty (from snapshot).
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Create a new handle with updated items (called from DD output stream).
    pub fn with_items(&self, items: Vec<Value>) -> Self {
        Self {
            id: self.id,
            cell_id: self.cell_id.clone(),
            items: Arc::new(items),
        }
    }

    /// Get an iterator over items (from snapshot).
    pub fn iter(&self) -> impl Iterator<Item = &Value> {
        self.items.iter()
    }
}

/// A simple value type for DD dataflows.
///
/// These values are pure data - no actors, no channels, no async.
/// They can be cloned, compared, and serialized.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Value {
    /// Null/unit value
    Unit,
    /// Boolean
    Bool(bool),
    /// Number (using OrderedFloat for Ord/Hash impl)
    Number(OrderedFloat<f64>),
    /// Text string
    Text(Arc<str>),
    /// Object (key-value pairs, ordered for Ord impl)
    Object(Arc<BTreeMap<Arc<str>, Value>>),
    /// List of values
    List(Arc<Vec<Value>>),
    /// DD Collection - provides O(delta) list operations via incremental computation.
    Collection(CollectionHandle),
    /// Tagged object (like Object but with a tag name)
    Tagged {
        tag: Arc<str>,
        fields: Arc<BTreeMap<Arc<str>, Value>>,
    },
    /// Reference to a HOLD state - resolved at render time
    CellRef(CellId),
    /// Reference to a LINK event source - used for reactive event wiring
    LinkRef(LinkId),
    /// Reference to a Timer event source - used for timer-triggered reactivity
    /// Contains (timer_id, interval_ms)
    TimerRef { id: Arc<str>, interval_ms: u64 },
    // ═══════════════════════════════════════════════════════════════════════════
    // SURGICALLY REMOVED (Phase 6.2): 13 symbolic reference Value variants
    //   WhileRef, ComputedRef, FilteredListRef, FilteredListRefWithPredicate,
    //   ReactiveFilteredList, ReactiveText, PlaceholderField, MappedListRef,
    //   FilteredMappedListRef, FilteredMappedListWithPredicate, PlaceholderWhileRef,
    //   NegatedPlaceholderField, LatestRef
    //
    // These variants stored "deferred evaluation recipes" that the bridge
    // evaluated imperatively at render time, bypassing DD dataflow.
    //
    // Phase 7 TODO: Replace with DD-native patterns:
    //   - WhileRef → DD join with predicate stream
    //   - ComputedRef → DD computed collection operators
    //   - *ListRef variants → DD collection handles with filter/map operators
    //   - ReactiveText → DD text concatenation operator
    //   - LatestRef → DD stream merge (concat) operator
    //   - All reactive values should be DD collection handles
    // ═══════════════════════════════════════════════════════════════════════════
    /// Placeholder for template substitution.
    /// Used in MappedListRef to mark where list item values should be inserted.
    /// At render time, the bridge substitutes actual item values for placeholders.
    Placeholder,
    /// Flushed value wrapper - propagates through pipelines for fail-fast error handling.
    ///
    /// When a FLUSH expression triggers, it wraps the value in Flushed.
    /// Operators should check for Flushed and propagate unchanged until
    /// caught by a FLUSH { ... } block.
    ///
    /// # Example
    /// ```ignore
    /// // value |> FLUSH { error_value }
    /// // becomes Flushed(error_value) that propagates through pipeline
    ///
    /// // FLUSH { pipeline } catches flushed values
    /// // extracts the inner value when Flushed is encountered
    /// ```
    Flushed(Box<Value>),
}

impl Value {
    /// Create a unit value.
    pub fn unit() -> Self {
        Self::Unit
    }

    /// Create an integer value.
    pub fn int(n: i64) -> Self {
        Self::Number(OrderedFloat(n as f64))
    }

    /// Create a float value.
    pub fn float(n: f64) -> Self {
        Self::Number(OrderedFloat(n))
    }

    /// Create a text value.
    pub fn text(s: impl Into<Arc<str>>) -> Self {
        Self::Text(s.into())
    }

    /// Create a LINK reference with the given path/id.
    pub fn link_ref(path: impl Into<String>) -> Self {
        Self::LinkRef(LinkId::new(path))
    }

    /// Create a Timer reference with the given id and interval.
    pub fn timer_ref(id: impl Into<Arc<str>>, interval_ms: u64) -> Self {
        Self::TimerRef { id: id.into(), interval_ms }
    }

    /// Create an object from key-value pairs.
    pub fn object(pairs: impl IntoIterator<Item = (impl Into<Arc<str>>, Value)>) -> Self {
        let map: BTreeMap<Arc<str>, Value> = pairs
            .into_iter()
            .map(|(k, v)| (k.into(), v))
            .collect();
        Self::Object(Arc::new(map))
    }

    /// Create a list from values.
    pub fn list(items: impl IntoIterator<Item = Value>) -> Self {
        Self::List(Arc::new(items.into_iter().collect()))
    }

    /// Create a DD collection from values - provides O(delta) incremental operations.
    pub fn collection(items: impl IntoIterator<Item = Value>) -> Self {
        Self::Collection(CollectionHandle::new(items.into_iter().collect()))
    }

    /// Create a DD collection linked to a cell for mutations.
    pub fn collection_with_cell(items: impl IntoIterator<Item = Value>, cell_id: impl Into<String>) -> Self {
        Self::Collection(CollectionHandle::with_cell(items.into_iter().collect(), cell_id))
    }

    /// Convert a List to a Collection (upgrade for incremental operations).
    pub fn to_collection(&self) -> Option<Self> {
        match self {
            Self::List(items) => Some(Self::Collection(CollectionHandle::new(items.as_ref().clone()))),
            Self::Collection(_) => Some(self.clone()),
            _ => None,
        }
    }

    /// Get items from either List or Collection.
    pub fn as_list_items(&self) -> Option<&[Value]> {
        match self {
            Self::List(items) => Some(items.as_slice()),
            Self::Collection(handle) => Some(handle.items()),
            _ => None,
        }
    }

    /// Check if this value is a list-like type (List or Collection).
    pub fn is_list_like(&self) -> bool {
        matches!(self, Self::List(_) | Self::Collection(_))
    }

    /// Create a tagged object.
    pub fn tagged(
        tag: impl Into<Arc<str>>,
        fields: impl IntoIterator<Item = (impl Into<Arc<str>>, Value)>,
    ) -> Self {
        let map: BTreeMap<Arc<str>, Value> = fields
            .into_iter()
            .map(|(k, v)| (k.into(), v))
            .collect();
        Self::Tagged {
            tag: tag.into(),
            fields: Arc::new(map),
        }
    }

    /// Get a field from an object or tagged object.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Self::Object(map) => map.get(key),
            Self::Tagged { fields, .. } => fields.get(key),
            _ => None,
        }
    }

    /// Create a new value with a field updated.
    /// Returns a clone of self if not an object/tagged type.
    pub fn with_field(&self, key: &str, value: Value) -> Self {
        match self {
            Self::Object(map) => {
                let mut new_map = (**map).clone();
                new_map.insert(Arc::from(key), value);
                Self::Object(Arc::new(new_map))
            }
            Self::Tagged { tag, fields } => {
                let mut new_fields = (**fields).clone();
                new_fields.insert(Arc::from(key), value);
                Self::Tagged {
                    tag: tag.clone(),
                    fields: Arc::new(new_fields),
                }
            }
            _ => self.clone(),
        }
    }

    /// Check if this value represents an undefined/unset state.
    pub fn is_undefined(&self) -> bool {
        matches!(self, Self::Unit)
    }

    /// Check if this is a truthy value.
    ///
    /// For CellRef values, looks up the actual stored value from CELL_STATES.
    /// This is important for correct evaluation of patterns like:
    ///   `List/retain(item, if: item.completed)` where completed is a CellRef.
    pub fn is_truthy(&self) -> bool {
        match self {
            Self::Unit => false,
            Self::Bool(b) => *b,
            Self::Number(n) => n.0 != 0.0,
            Self::Text(s) => !s.is_empty(),
            Self::Object(map) => !map.is_empty(),
            Self::List(items) => !items.is_empty(),
            Self::Collection(handle) => !handle.is_empty(),
            // False tag is falsy, True and all other tags are truthy
            Self::Tagged { tag, .. } => tag.as_ref() != "False",
            // CellRef - look up actual value from CELL_STATES
            Self::CellRef(cell_id) => {
                // Use the io module's get_cell_value to look up the actual value
                super::super::io::get_cell_value(&cell_id.name())
                    .map(|v| v.is_truthy())
                    .unwrap_or(false)
            }
            // LinkRef is truthy (represents an event source)
            Self::LinkRef(_) => true,
            // TimerRef is truthy (represents a timer event source)
            Self::TimerRef { .. } => true,
            // Placeholder - always truthy (represents a value slot)
            Self::Placeholder => true,
            // Flushed - truthy based on inner value
            Self::Flushed(inner) => inner.is_truthy(),
        }
    }

    /// Convert to display string for rendering.
    pub fn to_display_string(&self) -> String {
        match self {
            Self::Unit => String::new(),
            Self::Bool(b) => b.to_string(),
            Self::Number(n) => {
                // Display integers without decimal point
                if n.0.fract() == 0.0 && n.0.abs() < i64::MAX as f64 {
                    format!("{}", n.0 as i64)
                } else {
                    n.0.to_string()
                }
            }
            Self::Text(s) => s.to_string(),
            Self::Object(_) => "[object]".to_string(),
            Self::List(items) => format!("[list of {}]", items.len()),
            Self::Collection(handle) => format!("[collection of {}]", handle.len()),
            Self::Tagged { tag, .. } => format!("[{tag}]"),
            Self::CellRef(cell_id) => {
                // Resolve CellRef to actual HOLD value for display
                super::super::io::get_cell_value(&cell_id.name())
                    .map(|v| v.to_display_string())
                    .unwrap_or_else(|| String::new())
            }
            Self::LinkRef(link_id) => format!("[link:{}]", link_id.name()),
            Self::TimerRef { id, interval_ms } => format!("[timer:{}@{}ms]", id, interval_ms),
            Self::Placeholder => "[placeholder]".to_string(),
            Self::Flushed(inner) => {
                // Flushed values display their inner value with a marker
                format!("[flushed:{}]", inner.to_display_string())
            }
        }
    }

    /// Try to get as integer.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Number(n) => Some(n.0 as i64),
            _ => None,
        }
    }

    /// Try to get as float.
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Self::Number(n) => Some(n.0),
            _ => None,
        }
    }

    /// Try to get as text.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s),
            _ => None,
        }
    }

    /// Try to get as list.
    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Self::List(items) => Some(items),
            _ => None,
        }
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // SURGICALLY REMOVED (Phase 6.4): Template substitution methods
    //   - computed_ref() - constructor for removed ComputedRef variant
    //   - mapped_list_ref() - constructor for removed MappedListRef variant
    //   - substitute_placeholder() - template expansion method
    //   - substitute_placeholder_with_hover_remap() - template expansion with hover
    //
    // Phase 7 TODO: Replace with DD-native list rendering:
    //   - DD list_map operator creates elements from collection diffs
    //   - No template cloning needed - DD handles incremental updates
    // ═══════════════════════════════════════════════════════════════════════════
}

// ═══════════════════════════════════════════════════════════════════════════
// SURGICALLY REMOVED (Phase 6.4): Template and computation helper functions
//   - find_template_hover_link() - template inspection
//   - find_template_hover_cell() - template inspection
//   - evaluate_computed() - imperative computation evaluation
//   - resolve_computed_operand() - nested computation resolution
//
// These functions supported the removed symbolic reference types and
// performed imperative computations that bypassed DD dataflow.
//
// Phase 7 TODO: Replace with DD-native operators:
//   - DD collection handles provide reactive data access
//   - DD operators (count, filter, map) replace imperative computations
//   - DD output streams replace template inspection
// ═══════════════════════════════════════════════════════════════════════════

impl Default for Value {
    fn default() -> Self {
        Self::Unit
    }
}

impl From<i64> for Value {
    fn from(n: i64) -> Self {
        Self::int(n)
    }
}

impl From<i32> for Value {
    fn from(n: i32) -> Self {
        Self::int(n as i64)
    }
}

impl From<f64> for Value {
    fn from(n: f64) -> Self {
        Self::float(n)
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Self::Bool(b)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Self::Text(Arc::from(s))
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Self::Text(Arc::from(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_value_creation() {
        let unit = Value::unit();
        let num = Value::int(42);
        let float_num = Value::float(3.14);
        let text = Value::text("hello");
        let obj = Value::object([("x", Value::int(1)), ("y", Value::int(2))]);
        let list = Value::list([Value::int(1), Value::int(2), Value::int(3)]);

        assert_eq!(unit, Value::Unit);
        assert_eq!(num.as_int(), Some(42));
        assert_eq!(float_num.as_float(), Some(3.14));
        assert_eq!(text.as_text(), Some("hello"));
        assert_eq!(obj.get("x"), Some(&Value::int(1)));
        assert_eq!(list.as_list().map(|l| l.len()), Some(3));
    }

    #[test]
    fn test_value_ordering() {
        // Values must be Ord for DD
        let a = Value::int(1);
        let b = Value::int(2);
        assert!(a < b);

        let x = Value::text("a");
        let y = Value::text("b");
        assert!(x < y);
    }

    #[test]
    fn test_value_display() {
        assert_eq!(Value::int(42).to_display_string(), "42");
        assert_eq!(Value::float(3.14).to_display_string(), "3.14");
        assert_eq!(Value::text("hello").to_display_string(), "hello");
        assert_eq!(Value::Bool(true).to_display_string(), "true");
    }
}
