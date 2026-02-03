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

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use ordered_float::OrderedFloat;

use super::types::{CellId, LinkId, ITEM_KEY_FIELD};

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
/// - Is this value data or an update command?
/// - Transforms returned `Value` but sometimes it was actually an operation
///
/// Now operations are clearly separated:
/// - `Value`: Pure data that can be stored, compared, serialized
/// - `CellUpdate`: Commands that mutate state
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

    /// Insert an item into a list cell at index. O(n) shift.
    ListInsertAt {
        cell_id: Arc<str>,
        index: usize,
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
    /// Used to emit batch removals instead of reading IO state.
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

    /// Create a ListInsertAt update.
    pub fn list_insert_at(cell_id: impl Into<Arc<str>>, index: usize, item: Value) -> Self {
        Self::ListInsertAt {
            cell_id: cell_id.into(),
            index,
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
            Self::ListInsertAt { cell_id, .. } => Some(cell_id),
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

    /// String name used in cell IDs when collection outputs become scalar cells.
    pub fn name(&self) -> String {
        self.to_string()
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
#[derive(Clone, Debug)]
pub struct CollectionHandle {
    /// Unique identifier for this collection in the DD graph
    pub id: CollectionId,
    /// The cell ID where this collection's state is stored (for mutations)
    pub cell_id: Option<Arc<str>>,
}

impl CollectionHandle {
    /// Create a new collection handle (ID-only).
    pub fn new() -> Self {
        Self {
            id: CollectionId::new(),
            cell_id: None,
        }
    }

    /// Create a new collection handle linked to a cell (ID-only).
    pub fn new_with_cell_id(cell_id: impl Into<Arc<str>>) -> Self {
        Self {
            id: CollectionId::new(),
            cell_id: Some(cell_id.into()),
        }
    }

    /// Create a collection handle for a DD-registered collection.
    /// Used by evaluator helpers when registering DD operators.
    /// This is ID-only and intentionally unbound (`cell_id = None`):
    /// list state lives in ListState/SignalVec, and cell binding is explicit.
    pub fn new_with_id(id: CollectionId) -> Self {
        Self {
            id,
            cell_id: None,
        }
    }

    /// Create a collection handle with explicit id and cell id (ID-only).
    pub fn with_id_and_cell(id: CollectionId, cell_id: impl Into<Arc<str>>) -> Self {
        Self {
            id,
            cell_id: Some(cell_id.into()),
        }
    }

}

impl PartialEq for CollectionHandle {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.cell_id == other.cell_id
    }
}

impl Eq for CollectionHandle {}

impl PartialOrd for CollectionHandle {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CollectionHandle {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.id, &self.cell_id).cmp(&(other.id, &other.cell_id))
    }
}

impl std::hash::Hash for CollectionHandle {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
        self.cell_id.hash(state);
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
/// - Pure data types: Unit, Bool, Number, Text, Object, List, Tagged
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
    /// DD List - provides O(delta) list operations via incremental computation.
    List(CollectionHandle),
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
    /// Placeholder field access (e.g., item.field.subfield) used in templates.
    /// Resolved during template substitution/cloning.
    PlaceholderField(Arc<Vec<Arc<str>>>),
    /// Reactive WHILE configuration driven by a placeholder field.
    /// Resolved to WhileConfig during template substitution.
    PlaceholderWhile(Arc<PlaceholderWhileConfig>),
    /// Reactive WHILE configuration driven by a cell value.
    WhileConfig(Arc<WhileConfig>),
    /// Flushed value wrapper - propagates through pipelines for fail-fast error handling.
    ///
    /// When a FLUSH expression triggers, it wraps the value in Flushed.
    /// Operators should check for Flushed and propagate unchanged until
    /// caught by a FLUSH { ... } block.
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

    /// Create a DD list handle linked to a cell (ID-only).
    pub fn list_with_cell(cell_id: impl Into<String>) -> Self {
        Self::List(CollectionHandle::new_with_cell_id(Arc::from(cell_id.into())))
    }

    /// Check if this value is a list-like type (List only).
    pub fn is_list_like(&self) -> bool {
        matches!(self, Self::List(_))
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
    /// CellRef values are not resolved here; they must be handled by DD operators.
    pub fn is_truthy(&self) -> bool {
        match self {
            Self::Unit => false,
            Self::Bool(b) => *b,
            Self::Number(n) => n.0 != 0.0,
            Self::Text(s) => !s.is_empty(),
            Self::Object(map) => !map.is_empty(),
            Self::List(_) => {
                panic!("[DD Value] is_truthy called on List without resolved list state");
            }
            // False tag is falsy, True and all other tags are truthy
            Self::Tagged { tag, .. } => tag.as_ref() != "False",
            // CellRef requires resolved value; evaluator must not inspect IO state here.
            Self::CellRef(cell_id) => {
                panic!("[DD Value] is_truthy called on CellRef {} without resolved value", cell_id.name());
            }
            // LinkRef is truthy (represents an event source)
            Self::LinkRef(_) => true,
            // TimerRef is truthy (represents a timer event source)
            Self::TimerRef { .. } => true,
            // Placeholder - always truthy (represents a value slot)
            Self::Placeholder => true,
            Self::PlaceholderField(_) => {
                panic!("[DD Value] is_truthy called on PlaceholderField without substitution");
            }
            Self::WhileConfig(_) => {
                panic!("[DD Value] is_truthy called on WhileConfig; evaluate via DD/render");
            }
            Self::PlaceholderWhile(_) => {
                panic!("[DD Value] is_truthy called on PlaceholderWhile without substitution");
            }
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
            Self::List(handle) => match &handle.cell_id {
                Some(cell_id) => format!("[list:{}]", cell_id),
                None => "[list]".to_string(),
            },
            Self::Tagged { tag, .. } => format!("[{tag}]"),
            Self::CellRef(cell_id) => {
                panic!("[DD Value] to_display_string called on CellRef {} without resolved value", cell_id.name());
            }
            Self::LinkRef(link_id) => format!("[link:{}]", link_id.name()),
            Self::TimerRef { id, interval_ms } => format!("[timer:{}@{}ms]", id, interval_ms),
            Self::Placeholder => "[placeholder]".to_string(),
            Self::PlaceholderField(path) => {
                format!("[placeholder_field:{:?}]", path)
            }
            Self::WhileConfig(_) => "[while]".to_string(),
            Self::PlaceholderWhile(_) => "[placeholder_while]".to_string(),
            Self::Flushed(inner) => {
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

    /// Substitute Placeholder values with a concrete item value.
    /// Used by DD map operations when rendering collection items.
    pub fn substitute_placeholder(&self, item: &Value) -> Value {
        match self {
            Self::Placeholder => item.clone(),
            Self::PlaceholderField(path) => resolve_placeholder_field(item, path),
            Self::WhileConfig(config) => {
                let substituted = substitute_while_config(config, item);
                Value::WhileConfig(Arc::new(substituted))
            }
            Self::PlaceholderWhile(config) => resolve_placeholder_while(item, config),
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
            // Atomic values - no substitution needed
            _ => self.clone(),
        }
    }

    /// Substitute Placeholder and __placeholder_field__ tags with concrete item values.
    pub fn substitute_placeholders(&self, item: &Value) -> Value {
        match self {
            Self::Placeholder => item.clone(),
            Self::PlaceholderField(path) => resolve_placeholder_field(item, path),
            Self::WhileConfig(config) => {
                let substituted = substitute_while_config(config, item);
                Value::WhileConfig(Arc::new(substituted))
            }
            Self::PlaceholderWhile(config) => resolve_placeholder_while(item, config),
            Self::Object(fields) => {
                let new_fields: BTreeMap<Arc<str>, Value> = fields.iter()
                    .map(|(k, v)| (k.clone(), v.substitute_placeholders(item)))
                    .collect();
                Self::Object(Arc::new(new_fields))
            }
            Self::Tagged { tag, fields } => {
                let new_fields: BTreeMap<Arc<str>, Value> = fields.iter()
                    .map(|(k, v)| (k.clone(), v.substitute_placeholders(item)))
                    .collect();
                Self::Tagged {
                    tag: tag.clone(),
                    fields: Arc::new(new_fields),
                }
            }
            _ => self.clone(),
        }
    }
}

fn resolve_placeholder_field(item: &Value, path: &[Arc<str>]) -> Value {
    let mut current = item;
    for segment in path {
        let segment = segment.as_ref();
        current = match current {
            Value::Object(fields) => fields.get(segment).unwrap_or_else(|| {
                panic!(
                    "[DD Value] placeholder field missing '{}' on {:?}",
                    segment, current
                )
            }),
            Value::Tagged { fields, .. } => fields.get(segment).unwrap_or_else(|| {
                panic!(
                    "[DD Value] placeholder field missing '{}' on {:?}",
                    segment, current
                )
            }),
            other => panic!(
                "[DD Value] placeholder field path on non-object value {:?}",
                other
            ),
        };
    }
    current.clone()
}

fn substitute_while_config(config: &WhileConfig, item: &Value) -> WhileConfig {
    let arms: Vec<WhileArm> = config
        .arms
        .iter()
        .map(|arm| WhileArm {
            pattern: arm.pattern.substitute_placeholders(item),
            body: arm.body.substitute_placeholders(item),
        })
        .collect();
    let default = config.default.substitute_placeholders(item);
    WhileConfig {
        cell_id: config.cell_id.clone(),
        arms: Arc::new(arms),
        default: Box::new(default),
    }
}

fn resolve_placeholder_while(item: &Value, config: &PlaceholderWhileConfig) -> Value {
    let input = resolve_placeholder_field(item, &config.field_path);
    let arms: Vec<WhileArm> = config
        .arms
        .iter()
        .map(|arm| WhileArm {
            pattern: arm.pattern.substitute_placeholders(item),
            body: arm.body.substitute_placeholders(item),
        })
        .collect();
    let default = config.default.substitute_placeholders(item);

    match input {
        Value::CellRef(cell_id) => Value::WhileConfig(Arc::new(WhileConfig {
            cell_id,
            arms: Arc::new(arms),
            default: Box::new(default),
        })),
        Value::LinkRef(link_id) => {
            panic!(
                "[DD Value] Placeholder WHILE resolved to LinkRef {}; use a CellRef-backed state for WHILE in templates",
                link_id.name()
            );
        }
        other => {
            for arm in arms {
                if pattern_matches_value(&arm.pattern, &other) {
                    return arm.body;
                }
            }
            if !matches!(default, Value::Unit) {
                return default;
            }
            Value::Unit
        }
    }
}

fn pattern_matches_value(pattern: &Value, value: &Value) -> bool {
    use super::types::BoolTag;
    match (value, pattern) {
        (Value::Bool(b), Value::Tagged { tag, .. }) if BoolTag::is_bool_tag(tag.as_ref()) => {
            BoolTag::matches_bool(tag.as_ref(), *b)
        }
        _ => value == pattern,
    }
}

// ============================================================================
// WHILE CONFIG (internal runtime representation)
// ============================================================================

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WhileArm {
    pub pattern: Value,
    pub body: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WhileConfig {
    pub cell_id: CellId,
    pub arms: Arc<Vec<WhileArm>>,
    pub default: Box<Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PlaceholderWhileConfig {
    pub field_path: Arc<Vec<Arc<str>>>,
    pub arms: Arc<Vec<WhileArm>>,
    pub default: Box<Value>,
}

// ============================================================================
// LIST ITEM KEY UTILITIES (shared by worker/dataflow)
// ============================================================================

/// Get the __key field from an item fields map.
pub fn get_item_key_from_fields(fields: &BTreeMap<Arc<str>, Value>, context: &str) -> Arc<str> {
    match fields.get(ITEM_KEY_FIELD) {
        Some(Value::Text(key)) => key.clone(),
        Some(other) => panic!("[DD Value] __key must be Text in {}, found {:?}", context, other),
        None => panic!("[DD Value] missing __key in {}", context),
    }
}

/// Extract the __key from an item Value (Object or Tagged).
pub fn extract_item_key(value: &Value, context: &str) -> Arc<str> {
    match value {
        Value::Object(fields) => get_item_key_from_fields(fields, context),
        Value::Tagged { fields, .. } => get_item_key_from_fields(fields, context),
        other => panic!("[DD Value] {} item missing __key (expected Object/Tagged), found {:?}", context, other),
    }
}

/// Ensure all items have unique __key values.
pub fn ensure_unique_item_keys(items: &[Value], context: &str) {
    let mut seen: std::collections::HashSet<Arc<str>> = std::collections::HashSet::new();
    for item in items {
        let key = extract_item_key(item, context);
        if !seen.insert(key.clone()) {
            panic!("[DD Value] Duplicate __key '{}' in {}", key, context);
        }
    }
}

/// Attach a __key field to an item Value (Object or Tagged).
pub fn attach_item_key(value: Value, key: &str) -> Value {
    match value {
        Value::Object(fields) => {
            let mut new_fields = (*fields).clone();
            new_fields.insert(Arc::from(ITEM_KEY_FIELD), Value::text(key));
            Value::Object(Arc::new(new_fields))
        }
        Value::Tagged { tag, fields } => {
            let mut new_fields = (*fields).clone();
            new_fields.insert(Arc::from(ITEM_KEY_FIELD), Value::text(key));
            Value::Tagged { tag, fields: Arc::new(new_fields) }
        }
        other => {
            panic!("[DD Value] item must be Object or Tagged to attach __key, found {:?}", other);
        }
    }
}

/// Attach a __key if missing, or validate if present.
pub fn attach_or_validate_item_key(value: Value, source_key: &Arc<str>, context: &str) -> Value {
    match value {
        Value::Object(fields) => {
            if fields.contains_key(ITEM_KEY_FIELD) {
                let mapped_key = get_item_key_from_fields(&fields, context);
                if &mapped_key != source_key {
                    panic!(
                        "[DD Value] {} __key mismatch: '{}' != '{}'",
                        context, mapped_key, source_key
                    );
                }
                Value::Object(fields)
            } else {
                attach_item_key(Value::Object(fields), source_key)
            }
        }
        Value::Tagged { tag, fields } => {
            if fields.contains_key(ITEM_KEY_FIELD) {
                let mapped_key = get_item_key_from_fields(&fields, context);
                if &mapped_key != source_key {
                    panic!(
                        "[DD Value] {} __key mismatch: '{}' != '{}'",
                        context, mapped_key, source_key
                    );
                }
                Value::Tagged { tag, fields }
            } else {
                attach_item_key(Value::Tagged { tag, fields }, source_key)
            }
        }
        other => panic!("[DD Value] {} item must be Object/Tagged, found {:?}", context, other),
    }
}

/// Check if a value tree contains Placeholder markers.
pub fn contains_placeholder(value: &Value) -> bool {
    match value {
        Value::Placeholder => true,
        Value::PlaceholderField(_) => true,
        Value::PlaceholderWhile(_) => true,
        Value::WhileConfig(config) => {
            config.arms.iter().any(|arm| {
                contains_placeholder(&arm.pattern) || contains_placeholder(&arm.body)
            }) || contains_placeholder(&config.default)
        }
        Value::Object(fields) => fields.values().any(contains_placeholder),
        Value::Tagged { fields, .. } => fields.values().any(contains_placeholder),
        _ => false,
    }
}

/// Template-only wrapper for Values that may contain placeholders.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TemplateValue(pub Value);

impl TemplateValue {
    pub fn from_value(value: Value) -> Self {
        Self(value)
    }

    pub fn as_value(&self) -> &Value {
        &self.0
    }

    pub fn into_value(self) -> Value {
        self.0
    }

    pub fn contains_placeholder(&self) -> bool {
        contains_placeholder(&self.0)
    }

    pub fn substitute_placeholders(&self, item: &Value) -> Value {
        self.0.substitute_placeholders(item)
    }
}

impl From<Value> for TemplateValue {
    fn from(value: Value) -> Self {
        TemplateValue::from_value(value)
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
        let list = Value::list_with_cell("items");

        assert_eq!(unit, Value::Unit);
        assert_eq!(num.as_int(), Some(42));
        assert_eq!(float_num.as_float(), Some(3.14));
        assert_eq!(text.as_text(), Some("hello"));
        assert_eq!(obj.get("x"), Some(&Value::int(1)));
        assert!(list.is_list_like());
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

    #[test]
    fn test_collection_handle_new_with_id_is_unbound() {
        let id = CollectionId::new();
        let handle = CollectionHandle::new_with_id(id);
        assert_eq!(handle.id, id);
        assert!(handle.cell_id.is_none());
    }
}
