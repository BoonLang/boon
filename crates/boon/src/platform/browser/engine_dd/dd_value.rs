//! DD-compatible value types for Boon.
//!
//! These are simple data types that can flow through DD dataflows.
//! Unlike the actor-based `Value` in engine.rs, these are pure data.

use std::collections::BTreeMap;
use std::sync::Arc;

use ordered_float::OrderedFloat;

/// Type of computation for reactive computed values.
///
/// These represent "recipes" for computing values that depend on HOLD state.
/// The bridge evaluates these reactively when the source HOLD changes.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ComputedType {
    /// Count items in list where field == value
    /// Used for: `todos |> List/retain(item, if: item.completed) |> List/count()`
    ListCountWhere {
        field: Arc<str>,
        value: Box<DdValue>,
    },
    /// Total count of list items
    /// Used for: `todos |> List/count()`
    ListCount,
    /// Subtract two computed values
    /// Used for: `todos_count - completed_todos_count`
    Subtract {
        left: Box<DdValue>,
        right: Box<DdValue>,
    },
    /// Check if all items in list satisfy condition
    /// Used for: `todos |> List/all(item => item.completed)`
    ListAllWhere {
        field: Arc<str>,
        value: Box<DdValue>,
    },
    /// Check if computed value > 0
    /// Used for: `completed_todos_count > 0`
    GreaterThanZero {
        operand: Box<DdValue>,
    },
    /// Equality comparison of two values
    /// Used for: `active_todos_count == todos_count`
    Equal {
        left: Box<DdValue>,
        right: Box<DdValue>,
    },
    /// Count items in a list where items have HoldRef fields
    /// Used for: `todos |> List/retain(item, if: item.completed) |> List/count()`
    /// where each todo.completed is a HoldRef
    ReactiveListCountWhere {
        /// The list items (containing HoldRef fields)
        items: Arc<Vec<DdValue>>,
        /// The field to filter on
        field: Arc<str>,
        /// The value to match
        value: Box<DdValue>,
        /// The HoldRef IDs to observe (one per item)
        hold_ids: Arc<Vec<Arc<str>>>,
    },
}

/// A simple value type for DD dataflows.
///
/// These values are pure data - no actors, no channels, no async.
/// They can be cloned, compared, and serialized.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DdValue {
    /// Null/unit value
    Unit,
    /// Boolean
    Bool(bool),
    /// Number (using OrderedFloat for Ord/Hash impl)
    Number(OrderedFloat<f64>),
    /// Text string
    Text(Arc<str>),
    /// Object (key-value pairs, ordered for Ord impl)
    Object(Arc<BTreeMap<Arc<str>, DdValue>>),
    /// List of values
    List(Arc<Vec<DdValue>>),
    /// Tagged object (like Object but with a tag name)
    Tagged {
        tag: Arc<str>,
        fields: Arc<BTreeMap<Arc<str>, DdValue>>,
    },
    /// Reference to a HOLD state - resolved at render time
    HoldRef(Arc<str>),
    /// Reference to a LINK event source - used for reactive event wiring
    /// The string is the path to the LINK (e.g., "increment_button.event.press")
    LinkRef(Arc<str>),
    /// Reference to a Timer event source - used for timer-triggered reactivity
    /// Contains (timer_id, interval_ms)
    TimerRef { id: Arc<str>, interval_ms: u64 },
    /// Reactive WHILE/WHEN expression - evaluates arms based on HoldRef value
    /// Used when pattern matching against a reactive HoldRef input
    WhileRef {
        /// The HoldRef that provides the input value
        hold_id: Arc<str>,
        /// Optional computation to apply before matching (for ComputedRef inputs)
        /// If Some, the hold value is first passed through this computation,
        /// and the boolean result is matched against True/False patterns.
        computation: Option<ComputedType>,
        /// Pre-evaluated arms: (pattern_value, body_result)
        /// Each arm is (tag/value to match, evaluated body result)
        arms: Arc<Vec<(DdValue, DdValue)>>,
        /// Default arm result (for wildcard pattern)
        default: Option<Arc<DdValue>>,
    },
    /// Computed value that re-evaluates when dependencies change.
    /// Used for reactive expressions like `completed_todos_count` that need
    /// to update when a HOLD (like `todos`) changes.
    ComputedRef {
        /// The type of computation to perform
        computation: ComputedType,
        /// The HOLD ID this depends on (the primary data source)
        source_hold: Arc<str>,
    },
    /// Intermediate value for filtered list operations on HoldRef.
    /// Created when List/retain is applied to a HoldRef.
    /// Used to build ComputedRef when List/count() is called on this.
    FilteredListRef {
        /// The source HOLD ID containing the list
        source_hold: Arc<str>,
        /// The field to filter on (e.g., "completed")
        filter_field: Arc<str>,
        /// The value to match (e.g., Bool(true))
        filter_value: Box<DdValue>,
    },
    /// Reactive filtered list where items have HoldRef fields.
    /// Created when List/retain is applied to a concrete list
    /// where the filter condition references a field containing HoldRefs.
    /// Example: todos |> List/retain(item, if: item.completed)
    /// where each todo.completed is a HoldRef.
    ReactiveFilteredList {
        /// The concrete list items (containing HoldRef fields)
        items: Arc<Vec<DdValue>>,
        /// The field to filter on (e.g., "completed")
        filter_field: Arc<str>,
        /// The value to match (e.g., Bool(true))
        filter_value: Box<DdValue>,
        /// The HoldRef IDs that this list depends on (one per item)
        hold_ids: Arc<Vec<Arc<str>>>,
    },
}

impl DdValue {
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
    pub fn link_ref(path: impl Into<Arc<str>>) -> Self {
        Self::LinkRef(path.into())
    }

    /// Create a Timer reference with the given id and interval.
    pub fn timer_ref(id: impl Into<Arc<str>>, interval_ms: u64) -> Self {
        Self::TimerRef { id: id.into(), interval_ms }
    }

    /// Create an object from key-value pairs.
    pub fn object(pairs: impl IntoIterator<Item = (impl Into<Arc<str>>, DdValue)>) -> Self {
        let map: BTreeMap<Arc<str>, DdValue> = pairs
            .into_iter()
            .map(|(k, v)| (k.into(), v))
            .collect();
        Self::Object(Arc::new(map))
    }

    /// Create a list from values.
    pub fn list(items: impl IntoIterator<Item = DdValue>) -> Self {
        Self::List(Arc::new(items.into_iter().collect()))
    }

    /// Create a tagged object.
    pub fn tagged(
        tag: impl Into<Arc<str>>,
        fields: impl IntoIterator<Item = (impl Into<Arc<str>>, DdValue)>,
    ) -> Self {
        let map: BTreeMap<Arc<str>, DdValue> = fields
            .into_iter()
            .map(|(k, v)| (k.into(), v))
            .collect();
        Self::Tagged {
            tag: tag.into(),
            fields: Arc::new(map),
        }
    }

    /// Create a todo object with title and completed fields.
    /// Used for dynamically added todos in todo_mvc.
    pub fn todo_object(title: &str, completed: bool) -> Self {
        Self::object([
            ("title", Self::text(title)),
            ("completed", Self::Bool(completed)),
        ])
    }

    /// Get a field from an object or tagged object.
    pub fn get(&self, key: &str) -> Option<&DdValue> {
        match self {
            Self::Object(map) => map.get(key),
            Self::Tagged { fields, .. } => fields.get(key),
            _ => None,
        }
    }

    /// Check if this is a truthy value.
    ///
    /// For HoldRef values, looks up the actual stored value from HOLD_STATES.
    /// This is important for correct evaluation of patterns like:
    ///   `List/retain(item, if: item.completed)` where completed is a HoldRef.
    pub fn is_truthy(&self) -> bool {
        match self {
            Self::Unit => false,
            Self::Bool(b) => *b,
            Self::Number(n) => n.0 != 0.0,
            Self::Text(s) => !s.is_empty(),
            Self::Object(map) => !map.is_empty(),
            Self::List(items) => !items.is_empty(),
            // False tag is falsy, True and all other tags are truthy
            Self::Tagged { tag, .. } => tag.as_ref() != "False",
            // HoldRef - look up actual value from HOLD_STATES
            Self::HoldRef(hold_id) => {
                // Use the io module's get_hold_value to look up the actual value
                super::io::get_hold_value(hold_id)
                    .map(|v| v.is_truthy())
                    .unwrap_or(false)
            }
            // LinkRef is truthy (represents an event source)
            Self::LinkRef(_) => true,
            // TimerRef is truthy (represents a timer event source)
            Self::TimerRef { .. } => true,
            // WhileRef is truthy (represents a reactive expression)
            Self::WhileRef { .. } => true,
            // ComputedRef - evaluate computation with current HOLD state
            Self::ComputedRef { computation, source_hold } => {
                super::io::get_hold_value(source_hold)
                    .map(|source_value| {
                        evaluate_computed(computation, &source_value).is_truthy()
                    })
                    .unwrap_or(false)
            }
            // FilteredListRef - intermediate value, truthy if source exists
            Self::FilteredListRef { source_hold, .. } => {
                super::io::get_hold_value(source_hold).is_some()
            }
            // ReactiveFilteredList - truthy if it has items
            Self::ReactiveFilteredList { items, .. } => !items.is_empty(),
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
            Self::Tagged { tag, .. } => format!("[{tag}]"),
            Self::HoldRef(name) => format!("[hold:{}]", name),
            Self::LinkRef(path) => format!("[link:{}]", path),
            Self::TimerRef { id, interval_ms } => format!("[timer:{}@{}ms]", id, interval_ms),
            Self::WhileRef { hold_id, arms, .. } => format!("[while:{}#{}]", hold_id, arms.len()),
            Self::ComputedRef { computation, source_hold } => {
                format!("[computed:{:?}@{}]", computation, source_hold)
            }
            Self::FilteredListRef { source_hold, filter_field, .. } => {
                format!("[filtered:{}@{}]", filter_field, source_hold)
            }
            Self::ReactiveFilteredList { items, filter_field, .. } => {
                format!("[reactive-filtered:{}#{}]", filter_field, items.len())
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
    pub fn as_list(&self) -> Option<&[DdValue]> {
        match self {
            Self::List(items) => Some(items),
            _ => None,
        }
    }

    /// Create a computed reference value.
    pub fn computed_ref(computation: ComputedType, source_hold: impl Into<Arc<str>>) -> Self {
        Self::ComputedRef {
            computation,
            source_hold: source_hold.into(),
        }
    }
}

/// Evaluate a computed expression with a given source value.
///
/// This is called both from `is_truthy()` for synchronous evaluation
/// and from the bridge for reactive rendering.
pub fn evaluate_computed(computation: &ComputedType, source_value: &DdValue) -> DdValue {
    match computation {
        ComputedType::ListCount => {
            match source_value {
                DdValue::List(items) => DdValue::int(items.len() as i64),
                _ => DdValue::int(0),
            }
        }
        ComputedType::ListCountWhere { field, value } => {
            match source_value {
                DdValue::List(items) => {
                    let count = items.iter()
                        .filter(|item| {
                            if let DdValue::Object(obj) = item {
                                obj.get(field.as_ref()) == Some(value.as_ref())
                            } else {
                                false
                            }
                        })
                        .count();
                    DdValue::int(count as i64)
                }
                _ => DdValue::int(0),
            }
        }
        ComputedType::ListAllWhere { field, value } => {
            match source_value {
                DdValue::List(items) => {
                    if items.is_empty() {
                        DdValue::Bool(false)
                    } else {
                        let all_match = items.iter().all(|item| {
                            if let DdValue::Object(obj) = item {
                                obj.get(field.as_ref()) == Some(value.as_ref())
                            } else {
                                false
                            }
                        });
                        DdValue::Bool(all_match)
                    }
                }
                _ => DdValue::Bool(false),
            }
        }
        ComputedType::Subtract { left, right } => {
            // Resolve left and right values
            let left_val = resolve_computed_operand(left, source_value);
            let right_val = resolve_computed_operand(right, source_value);

            match (left_val.as_int(), right_val.as_int()) {
                (Some(l), Some(r)) => DdValue::int(l - r),
                _ => DdValue::int(0),
            }
        }
        ComputedType::GreaterThanZero { operand } => {
            let val = resolve_computed_operand(operand, source_value);
            match val.as_int() {
                Some(n) => DdValue::Bool(n > 0),
                None => DdValue::Bool(false),
            }
        }
        ComputedType::Equal { left, right } => {
            let left_val = resolve_computed_operand(left, source_value);
            let right_val = resolve_computed_operand(right, source_value);
            DdValue::Bool(left_val == right_val)
        }
        ComputedType::ReactiveListCountWhere { items: static_items, field: _, value, hold_ids: _ } => {
            // Count items where the "completed" field matches the expected value
            // IMPORTANT: Use current "todos" HOLD as source of truth (not static items)
            // This handles Clear completed removing items dynamically

            let current_items = super::io::get_hold_value("todos")
                .and_then(|v| match v {
                    DdValue::List(list) => Some(list),
                    _ => None,
                })
                .unwrap_or_else(|| static_items.clone());

            let count = current_items.iter()
                .filter(|item| {
                    // Get the completed field value
                    let completed_value = match item {
                        DdValue::Object(obj) => {
                            match obj.get("completed") {
                                // Direct Bool value (dynamic todos)
                                Some(DdValue::Bool(b)) => Some(DdValue::Bool(*b)),
                                // HoldRef - look up current value
                                Some(DdValue::HoldRef(hold_id)) => {
                                    super::io::get_hold_value(hold_id)
                                }
                                _ => None,
                            }
                        }
                        _ => None,
                    };

                    // Compare with expected value
                    match (completed_value.as_ref(), value.as_ref()) {
                        (Some(DdValue::Bool(b)), DdValue::Bool(expected)) => *b == *expected,
                        (Some(DdValue::Tagged { tag, .. }), DdValue::Tagged { tag: expected_tag, .. }) => {
                            tag == expected_tag
                        }
                        (Some(current), expected) => current == expected,
                        _ => false,
                    }
                })
                .count();
            DdValue::int(count as i64)
        }
    }
}

/// Resolve a computed operand - if it's a nested ComputedRef, evaluate it.
fn resolve_computed_operand(value: &DdValue, source_value: &DdValue) -> DdValue {
    match value {
        DdValue::ComputedRef { computation, source_hold: _ } => {
            // For nested computations, use the same source value
            // (they all depend on the same HOLD)
            evaluate_computed(computation, source_value)
        }
        DdValue::HoldRef(hold_id) => {
            // Look up from global state
            super::io::get_hold_value(hold_id).unwrap_or(DdValue::Unit)
        }
        _ => value.clone(),
    }
}

impl Default for DdValue {
    fn default() -> Self {
        Self::Unit
    }
}

impl From<i64> for DdValue {
    fn from(n: i64) -> Self {
        Self::int(n)
    }
}

impl From<i32> for DdValue {
    fn from(n: i32) -> Self {
        Self::int(n as i64)
    }
}

impl From<f64> for DdValue {
    fn from(n: f64) -> Self {
        Self::float(n)
    }
}

impl From<bool> for DdValue {
    fn from(b: bool) -> Self {
        Self::Bool(b)
    }
}

impl From<&str> for DdValue {
    fn from(s: &str) -> Self {
        Self::Text(Arc::from(s))
    }
}

impl From<String> for DdValue {
    fn from(s: String) -> Self {
        Self::Text(Arc::from(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_value_creation() {
        let unit = DdValue::unit();
        let num = DdValue::int(42);
        let float_num = DdValue::float(3.14);
        let text = DdValue::text("hello");
        let obj = DdValue::object([("x", DdValue::int(1)), ("y", DdValue::int(2))]);
        let list = DdValue::list([DdValue::int(1), DdValue::int(2), DdValue::int(3)]);

        assert_eq!(unit, DdValue::Unit);
        assert_eq!(num.as_int(), Some(42));
        assert_eq!(float_num.as_float(), Some(3.14));
        assert_eq!(text.as_text(), Some("hello"));
        assert_eq!(obj.get("x"), Some(&DdValue::int(1)));
        assert_eq!(list.as_list().map(|l| l.len()), Some(3));
    }

    #[test]
    fn test_value_ordering() {
        // Values must be Ord for DD
        let a = DdValue::int(1);
        let b = DdValue::int(2);
        assert!(a < b);

        let x = DdValue::text("a");
        let y = DdValue::text("b");
        assert!(x < y);
    }

    #[test]
    fn test_value_display() {
        assert_eq!(DdValue::int(42).to_display_string(), "42");
        assert_eq!(DdValue::float(3.14).to_display_string(), "3.14");
        assert_eq!(DdValue::text("hello").to_display_string(), "hello");
        assert_eq!(DdValue::Bool(true).to_display_string(), "true");
    }
}
