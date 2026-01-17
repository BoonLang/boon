//! DD-compatible value types for Boon.
//!
//! These are simple data types that can flow through DD dataflows.
//! Unlike the actor-based `Value` in engine.rs, these are pure data.
//!
//! # Pure DD Design (Phase 7)
//!
//! This module contains ONLY pure data types. No symbolic references,
//! no deferred computations, no rendering hints. All computation
//! happens through DD operators in the dataflow graph.
//!
//! The bridge reads DD output streams to render UI - it does NOT
//! interpret symbolic references or perform imperative computation.

use std::collections::BTreeMap;
use std::sync::Arc;

use ordered_float::OrderedFloat;

use super::types::{CellId, LinkId};

// ============================================================================
// CELL UPDATE OPERATIONS - Phase 7.3: Separate operations from values
// ============================================================================
//
// CellUpdate represents state mutations that flow through DD without being
// confused with actual data values. This separation enables:
// - Pure transforms: always return data OR operations, never mixed
// - Clear semantics: operations are commands to the output observer
// - Future optimization: operations can be batched/coalesced
//
// The output observer (`sync_cell_from_dd`) dispatches on CellUpdate type
// to apply the appropriate mutation to CELL_STATES and MutableVec.

/// Operation to apply to a cell's state.
///
/// Unlike `Value`, which represents pure data, `CellUpdate` represents
/// state mutations. This enum is used as the return type for transforms
/// and consumed by the output observer.
///
/// # Phase 7.3: Pure DD Design
///
/// Previously, operations were mixed into the `Value` enum, causing confusion:
/// - Is `Value::ListPush` a value or a command?
/// - Transforms returned `Value` but sometimes it was actually an operation
///
/// Now operations are clearly separated:
/// - `Value`: Pure data that can be stored, compared, serialized
/// - `CellUpdate`: Commands that mutate state
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CellUpdate {
    /// Set cell to a new value (full replacement)
    SetValue {
        cell_id: Arc<str>,
        value: Value,
    },

    /// Append an item to a list cell. O(1) operation.
    ListPush {
        cell_id: Arc<str>,
        item: Value,
    },

    /// Remove an item from a list cell by index. O(n) shift but no clone.
    ListRemoveAt {
        cell_id: Arc<str>,
        index: usize,
    },

    /// Remove an item from a list cell by key (HoldRef/LinkRef ID). O(1) lookup.
    ListRemoveByKey {
        cell_id: Arc<str>,
        key: Arc<str>,
    },

    /// Remove multiple items from a list cell by keys. O(k) where k = keys.len().
    /// Used by ListRemoveCompleted to emit batch removals instead of reading IO state.
    ListRemoveBatch {
        cell_id: Arc<str>,
        keys: Vec<Arc<str>>,
    },

    /// Clear all items from a list cell. O(1) operation.
    ListClear {
        cell_id: Arc<str>,
    },

    /// Update a field on a list item identified by key. O(1) lookup + O(1) update.
    ListItemUpdate {
        cell_id: Arc<str>,
        key: Arc<str>,
        field_path: Arc<Vec<Arc<str>>>,
        new_value: Value,
    },

    /// Multiple cell updates to apply atomically.
    /// Used when a single transform needs to update multiple cells.
    Multi(Vec<CellUpdate>),

    /// No operation - used when a transform doesn't need to update state
    NoOp,
}

impl CellUpdate {
    /// Create a SetValue update.
    pub fn set_value(cell_id: impl Into<Arc<str>>, value: Value) -> Self {
        Self::SetValue {
            cell_id: cell_id.into(),
            value,
        }
    }

    /// Create a ListPush update.
    pub fn list_push(cell_id: impl Into<Arc<str>>, item: Value) -> Self {
        Self::ListPush {
            cell_id: cell_id.into(),
            item,
        }
    }

    /// Create a ListRemoveAt update.
    pub fn list_remove_at(cell_id: impl Into<Arc<str>>, index: usize) -> Self {
        Self::ListRemoveAt {
            cell_id: cell_id.into(),
            index,
        }
    }

    /// Create a ListRemoveByKey update.
    pub fn list_remove_by_key(cell_id: impl Into<Arc<str>>, key: impl Into<Arc<str>>) -> Self {
        Self::ListRemoveByKey {
            cell_id: cell_id.into(),
            key: key.into(),
        }
    }

    /// Create a ListRemoveBatch update.
    pub fn list_remove_batch(cell_id: impl Into<Arc<str>>, keys: Vec<Arc<str>>) -> Self {
        Self::ListRemoveBatch {
            cell_id: cell_id.into(),
            keys,
        }
    }

    /// Create a ListClear update.
    pub fn list_clear(cell_id: impl Into<Arc<str>>) -> Self {
        Self::ListClear {
            cell_id: cell_id.into(),
        }
    }

    /// Create a ListItemUpdate update.
    pub fn list_item_update(
        cell_id: impl Into<Arc<str>>,
        key: impl Into<Arc<str>>,
        field_path: Vec<Arc<str>>,
        new_value: Value,
    ) -> Self {
        Self::ListItemUpdate {
            cell_id: cell_id.into(),
            key: key.into(),
            field_path: Arc::new(field_path),
            new_value,
        }
    }

    /// Create a Multi update from a vector of updates.
    pub fn multi(updates: Vec<CellUpdate>) -> Self {
        Self::Multi(updates)
    }

    /// Check if this is a NoOp.
    pub fn is_noop(&self) -> bool {
        matches!(self, Self::NoOp)
    }

    /// Get the cell ID this update targets (for single-cell updates).
    pub fn cell_id(&self) -> Option<&str> {
        match self {
            Self::SetValue { cell_id, .. } => Some(cell_id),
            Self::ListPush { cell_id, .. } => Some(cell_id),
            Self::ListRemoveAt { cell_id, .. } => Some(cell_id),
            Self::ListRemoveByKey { cell_id, .. } => Some(cell_id),
            Self::ListRemoveBatch { cell_id, .. } => Some(cell_id),
            Self::ListClear { cell_id } => Some(cell_id),
            Self::ListItemUpdate { cell_id, .. } => Some(cell_id),
            Self::Multi(_) => None, // Multiple cells
            Self::NoOp => None,
        }
    }
}

/// Convert CellUpdate to Value for backward compatibility during migration.
///
/// This allows gradual migration from Value-based operations to CellUpdate.
/// Once migration is complete, the operation variants can be removed from Value.
impl From<CellUpdate> for Value {
    fn from(update: CellUpdate) -> Self {
        match update {
            CellUpdate::SetValue { value, .. } => value,
            CellUpdate::ListPush { cell_id, item } => Value::ListPush {
                cell_id,
                item: Box::new(item),
            },
            CellUpdate::ListRemoveAt { cell_id, index } => Value::ListRemoveAt { cell_id, index },
            CellUpdate::ListRemoveByKey { cell_id, key } => Value::ListRemoveByKey { cell_id, key },
            CellUpdate::ListRemoveBatch { cell_id, keys } => Value::ListRemoveBatch { cell_id, keys },
            CellUpdate::ListClear { cell_id } => Value::ListClear { cell_id },
            CellUpdate::ListItemUpdate { cell_id, key, field_path, new_value } => Value::ListItemUpdate {
                cell_id,
                key,
                field_path,
                new_value: Box::new(new_value),
            },
            CellUpdate::Multi(updates) => Value::MultiCellUpdate(
                updates.into_iter()
                    .filter_map(|u| {
                        match u {
                            CellUpdate::SetValue { cell_id, value } => Some((cell_id, Box::new(value))),
                            other => {
                                let cell_id = other.cell_id()?.to_string();
                                Some((Arc::from(cell_id), Box::new(Value::from(other))))
                            }
                        }
                    })
                    .collect()
            ),
            CellUpdate::NoOp => Value::Unit,
        }
    }
}

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

    /// Create a collection handle for a DD-registered collection.
    /// Used by evaluator helpers when registering DD operators.
    /// The items are initially empty - DD will populate them through the output stream.
    pub fn new_with_id(id: CollectionId) -> Self {
        Self {
            id,
            cell_id: None,
            items: Arc::new(Vec::new()),
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

// ============================================================================
// PURE DD VALUE ENUM
// ============================================================================

/// A simple value type for DD dataflows.
///
/// These values are pure data - no actors, no channels, no async.
/// They can be cloned, compared, and serialized.
///
/// # Phase 7: Pure DD
///
/// This enum contains ONLY:
/// - Pure data types: Unit, Bool, Number, Text, Object, List, Collection, Tagged
/// - Reference types: CellRef, LinkRef, TimerRef (resolved by DD output observer)
/// - Template support: Placeholder (for DD map operations)
/// - Error handling: Flushed (for fail-fast propagation)
///
/// NO symbolic reference variants (WhileRef, ComputedRef, FilteredListRef, etc.)
/// All computation happens through DD operators, not deferred evaluation.
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
    /// Reference to a HOLD state cell - DD output observer resolves this
    CellRef(CellId),
    /// Reference to a LINK event source - DD wires this to event handlers
    LinkRef(LinkId),
    /// Reference to a Timer event source - DD wires this to timer handlers
    /// Contains (timer_id, interval_ms)
    TimerRef { id: Arc<str>, interval_ms: u64 },
    /// Placeholder for DD map template substitution.
    /// Used in DD map operations to mark where collection items should be inserted.
    /// The DD map operator substitutes actual item values for placeholders.
    Placeholder,
    /// Flushed value wrapper - propagates through pipelines for fail-fast error handling.
    ///
    /// When a FLUSH expression triggers, it wraps the value in Flushed.
    /// Operators should check for Flushed and propagate unchanged until
    /// caught by a FLUSH { ... } block.
    Flushed(Box<Value>),

    // ========================================================================
    // LIST DIFF VARIANTS - Phase 2.1: O(delta) list operations
    // ========================================================================
    // These variants represent list mutations that flow through DD without
    // requiring full list clones. The output observer applies them directly
    // to MutableVec for incremental DOM updates.

    /// Append an item to a list cell. O(1) operation.
    /// The cell_id identifies which list to append to.
    /// Applied by sync_cell_from_dd() directly to MutableVec.
    ListPush {
        cell_id: Arc<str>,
        item: Box<Value>,
    },

    /// Remove an item from a list cell by index. O(n) shift but no clone.
    /// The cell_id identifies which list to modify.
    ListRemoveAt {
        cell_id: Arc<str>,
        index: usize,
    },

    /// Remove an item from a list cell by key (HoldRef/LinkRef ID). O(1) lookup.
    /// The cell_id identifies which list to modify.
    /// The key is extracted from HoldRef/LinkRef for O(1) HashMap lookup.
    ListRemoveByKey {
        cell_id: Arc<str>,
        key: Arc<str>,
    },

    /// Clear all items from a list cell. O(1) operation.
    /// The cell_id identifies which list to clear.
    ListClear {
        cell_id: Arc<str>,
    },

    /// Remove multiple items from a list cell by keys. O(k) where k = keys.len().
    /// Used by ListRemoveCompleted to emit batch removals instead of reading IO state.
    /// The cell_id identifies which list to modify.
    /// Keys are extracted from HoldRef/LinkRef for O(1) HashMap lookup per key.
    ListRemoveBatch {
        cell_id: Arc<str>,
        keys: Vec<Arc<str>>,
    },

    /// Update a field on a list item identified by key. O(1) lookup + O(1) update.
    /// Used for toggling checkboxes, updating text, etc.
    ListItemUpdate {
        cell_id: Arc<str>,
        key: Arc<str>,
        field_path: Arc<Vec<Arc<str>>>,
        new_value: Box<Value>,
    },

    // ========================================================================
    // MULTI-CELL UPDATE - Phase 2: Eliminate side effects in transforms
    // ========================================================================
    // When a transform needs to update multiple cells atomically (e.g., clearing
    // both a data list and its parallel elements list), it returns MultiCellUpdate
    // instead of calling update_cell_no_persist() as a side effect.
    // The output observer expands this into individual cell updates.

    /// Multiple cell updates to apply atomically.
    /// Used when a single transform needs to update multiple cells.
    /// The output observer expands this into individual sync_cell_from_dd calls.
    MultiCellUpdate(Vec<(Arc<str>, Box<Value>)>),
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

    // ========================================================================
    // LIST DIFF CONSTRUCTORS - Phase 2.1: O(delta) list operations
    // ========================================================================

    /// Create a ListPush diff - appends item to list cell. O(1) operation.
    /// Use this instead of cloning the full list and pushing.
    pub fn list_push(cell_id: impl Into<Arc<str>>, item: Value) -> Self {
        Self::ListPush {
            cell_id: cell_id.into(),
            item: Box::new(item),
        }
    }

    /// Create a ListRemoveAt diff - removes item at index. O(n) shift but no clone.
    pub fn list_remove_at(cell_id: impl Into<Arc<str>>, index: usize) -> Self {
        Self::ListRemoveAt {
            cell_id: cell_id.into(),
            index,
        }
    }

    /// Create a ListRemoveByKey diff - removes item by HoldRef/LinkRef key. O(1) lookup.
    pub fn list_remove_by_key(cell_id: impl Into<Arc<str>>, key: impl Into<Arc<str>>) -> Self {
        Self::ListRemoveByKey {
            cell_id: cell_id.into(),
            key: key.into(),
        }
    }

    /// Create a ListRemoveBatch diff - removes multiple items by keys. O(k) operation.
    /// Used when removing all completed items (ListRemoveCompleted) to emit batch removal
    /// instead of reading from IO layer.
    pub fn list_remove_batch(cell_id: impl Into<Arc<str>>, keys: Vec<Arc<str>>) -> Self {
        Self::ListRemoveBatch {
            cell_id: cell_id.into(),
            keys,
        }
    }

    /// Create a ListClear diff - clears all items. O(1) operation.
    pub fn list_clear(cell_id: impl Into<Arc<str>>) -> Self {
        Self::ListClear {
            cell_id: cell_id.into(),
        }
    }

    /// Create a ListItemUpdate diff - updates field on item by key. O(1) lookup + O(1) update.
    pub fn list_item_update(
        cell_id: impl Into<Arc<str>>,
        key: impl Into<Arc<str>>,
        field_path: Vec<Arc<str>>,
        new_value: Value,
    ) -> Self {
        Self::ListItemUpdate {
            cell_id: cell_id.into(),
            key: key.into(),
            field_path: Arc::new(field_path),
            new_value: Box::new(new_value),
        }
    }

    /// Create a MultiCellUpdate - atomic batch of cell updates.
    /// Used when a transform needs to update multiple cells (e.g., data list + elements list).
    /// The output observer expands this into individual sync_cell_from_dd calls.
    pub fn multi_cell_update(updates: Vec<(impl Into<Arc<str>>, Value)>) -> Self {
        Self::MultiCellUpdate(
            updates
                .into_iter()
                .map(|(cell_id, value)| (cell_id.into(), Box::new(value)))
                .collect()
        )
    }

    /// Check if this value is a list diff operation.
    pub fn is_list_diff(&self) -> bool {
        matches!(
            self,
            Self::ListPush { .. }
                | Self::ListRemoveAt { .. }
                | Self::ListRemoveByKey { .. }
                | Self::ListRemoveBatch { .. }
                | Self::ListClear { .. }
                | Self::ListItemUpdate { .. }
                | Self::MultiCellUpdate(_)
        )
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
    /// For CellRef values, looks up the actual stored value from cell state.
    /// This is important for correct evaluation of patterns in DD operators.
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
            // CellRef - look up actual value from cell state
            Self::CellRef(cell_id) => {
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
            // ListDiff variants are truthy (they represent pending operations)
            Self::ListPush { .. } => true,
            Self::ListRemoveAt { .. } => true,
            Self::ListRemoveByKey { .. } => true,
            Self::ListRemoveBatch { .. } => true,
            Self::ListClear { .. } => true,
            Self::ListItemUpdate { .. } => true,
            Self::MultiCellUpdate(_) => true,
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
                // Resolve CellRef to actual cell value for display
                super::super::io::get_cell_value(&cell_id.name())
                    .map(|v| v.to_display_string())
                    .unwrap_or_else(|| String::new())
            }
            Self::LinkRef(link_id) => format!("[link:{}]", link_id.name()),
            Self::TimerRef { id, interval_ms } => format!("[timer:{}@{}ms]", id, interval_ms),
            Self::Placeholder => "[placeholder]".to_string(),
            Self::Flushed(inner) => {
                format!("[flushed:{}]", inner.to_display_string())
            }
            // ListDiff variants - display as operations
            Self::ListPush { cell_id, item } => format!("[list_push:{} <- {}]", cell_id, item.to_display_string()),
            Self::ListRemoveAt { cell_id, index } => format!("[list_remove_at:{}[{}]]", cell_id, index),
            Self::ListRemoveByKey { cell_id, key } => format!("[list_remove_by_key:{}[{}]]", cell_id, key),
            Self::ListRemoveBatch { cell_id, keys } => format!("[list_remove_batch:{}[{} keys]]", cell_id, keys.len()),
            Self::ListClear { cell_id } => format!("[list_clear:{}]", cell_id),
            Self::ListItemUpdate { cell_id, key, field_path, new_value } => {
                format!("[list_item_update:{}[{}].{:?} = {}]", cell_id, key, field_path, new_value.to_display_string())
            }
            Self::MultiCellUpdate(updates) => format!("[multi_cell_update:{} updates]", updates.len()),
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

    /// Substitute Placeholder values with a concrete item value.
    /// Used by DD map operations when rendering collection items.
    pub fn substitute_placeholder(&self, item: &Value) -> Value {
        match self {
            Self::Placeholder => item.clone(),
            Self::Object(fields) => {
                let new_fields: BTreeMap<Arc<str>, Value> = fields.iter()
                    .map(|(k, v)| (k.clone(), v.substitute_placeholder(item)))
                    .collect();
                Self::Object(Arc::new(new_fields))
            }
            Self::Tagged { tag, fields } => {
                let new_fields: BTreeMap<Arc<str>, Value> = fields.iter()
                    .map(|(k, v)| (k.clone(), v.substitute_placeholder(item)))
                    .collect();
                Self::Tagged {
                    tag: tag.clone(),
                    fields: Arc::new(new_fields),
                }
            }
            Self::List(items) => {
                let new_items: Vec<Value> = items.iter()
                    .map(|v| v.substitute_placeholder(item))
                    .collect();
                Self::List(Arc::new(new_items))
            }
            // Atomic values - no substitution needed
            _ => self.clone(),
        }
    }
}

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

    #[test]
    fn test_placeholder_substitution() {
        let template = Value::object([
            ("name", Value::Placeholder),
            ("count", Value::int(5)),
        ]);
        let item = Value::text("Alice");
        let result = template.substitute_placeholder(&item);

        assert_eq!(result.get("name"), Some(&Value::text("Alice")));
        assert_eq!(result.get("count"), Some(&Value::int(5)));
    }
}
