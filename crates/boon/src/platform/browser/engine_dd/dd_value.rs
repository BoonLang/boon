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
    /// Used for: `items |> List/retain(item, if: item.completed) |> List/count()`
    ListCountWhere {
        field: Arc<str>,
        value: Box<DdValue>,
    },
    /// Total count of list items
    /// Used for: `items |> List/count()`
    ListCount,
    /// Subtract two computed values
    /// Used for: `list_count - completed_items_count`
    Subtract {
        left: Box<DdValue>,
        right: Box<DdValue>,
    },
    /// Check if all items in list satisfy condition
    /// Used for: `items |> List/all(item => item.completed)`
    ListAllWhere {
        field: Arc<str>,
        value: Box<DdValue>,
    },
    /// Check if computed value > 0
    /// Used for: `completed_items_count > 0`
    GreaterThanZero {
        operand: Box<DdValue>,
    },
    /// Equality comparison of two values
    /// Used for: `active_items_count == list_count`
    Equal {
        left: Box<DdValue>,
        right: Box<DdValue>,
    },
    // DELETED: ReactiveListCountWhere - was dead code (never constructed)
    // Use ListCountWhereHold instead which reads live HOLD data
    /// Count items in a list HOLD where field matches value.
    /// Reads LIVE data from source_hold on each evaluation.
    /// This handles both static and dynamic items correctly.
    ListCountWhereHold {
        /// Which HOLD contains the list (e.g., from variable context)
        source_hold: Arc<str>,
        /// The field to filter on (e.g., "completed")
        field: Arc<str>,
        /// The value to match (e.g., Bool(true))
        value: Box<DdValue>,
    },
    /// Total count of items in a list HOLD.
    /// Reads LIVE data from source_hold on each evaluation.
    /// Used for: `items |> List/count()` where items came from a HOLD variable.
    ListCountHold {
        /// Which HOLD contains the list
        source_hold: Arc<str>,
    },
    /// Check if a list HOLD is empty.
    /// Reads LIVE data from source_hold on each evaluation.
    /// Used for: `items |> List/is_empty()` where items came from a HOLD variable.
    ListIsEmptyHold {
        /// Which HOLD contains the list
        source_hold: Arc<str>,
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
    /// Used for reactive expressions like `completed_items_count` that need
    /// to update when a HOLD (like `items`) changes.
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
    /// Intermediate filtered list reference with predicate template.
    /// Created when List/retain has a complex predicate (not simple field=value).
    /// The predicate_template contains Placeholder markers for item values.
    FilteredListRefWithPredicate {
        /// The source HOLD ID containing the list
        source_hold: Arc<str>,
        /// The predicate template - contains Placeholder markers for item fields.
        /// At render time, for each item, substitute_placeholder is called.
        predicate_template: Box<DdValue>,
    },
    /// Reactive filtered list where items have HoldRef fields.
    /// Created when List/retain is applied to a concrete list
    /// where the filter condition references a field containing HoldRefs.
    /// Example: items |> List/retain(item, if: item.completed)
    /// where each item.completed is a HoldRef.
    ReactiveFilteredList {
        /// The concrete list items (containing HoldRef fields)
        items: Arc<Vec<DdValue>>,
        /// The field to filter on (e.g., "completed")
        filter_field: Arc<str>,
        /// The value to match (e.g., Bool(true))
        filter_value: Box<DdValue>,
        /// The HoldRef IDs that this list depends on (one per item)
        hold_ids: Arc<Vec<Arc<str>>>,
        /// The source HOLD containing this list (for live counting)
        source_hold: Arc<str>,
    },
    /// Reactive TEXT interpolation - evaluated at render time.
    /// Created when TEXT contains reactive parts (ComputedRef, WhileRef, HoldRef).
    /// Each part is either a literal Text or a reactive value.
    /// Example: TEXT { {count} items left } with reactive count
    ReactiveText {
        /// Parts of the text - can be Text (literal) or reactive values
        parts: Arc<Vec<DdValue>>,
    },
    /// Placeholder for template substitution.
    /// Used in MappedListRef to mark where list item values should be inserted.
    /// At render time, the bridge substitutes actual item values for placeholders.
    Placeholder,
    /// Deferred field access on a Placeholder.
    /// Created when evaluating `item.field` where `item` is a Placeholder.
    /// At render time, the path is resolved against the actual item value.
    /// Example: `item.completed` → `PlaceholderField { path: ["completed"] }`
    PlaceholderField {
        /// Field access path from the placeholder (e.g., ["todo_elements", "remove_button"])
        path: Arc<Vec<Arc<str>>>,
    },
    /// Mapped list reference - reactive List/map over a HoldRef.
    /// Created when: `hold_ref |> List/map(item, new: element_expr)`
    /// The element_template is evaluated once with `item` bound to Placeholder,
    /// then at render time, the Placeholder is substituted with each actual item.
    MappedListRef {
        /// The source HOLD ID containing the list
        source_hold: Arc<str>,
        /// Template element with Placeholder where the item value goes
        element_template: Arc<DdValue>,
    },
    /// Filtered and mapped list reference - reactive List/retain then List/map over a HoldRef.
    /// Created when: `hold_ref |> List/retain(item, if: ...) |> List/map(item, new: element_expr)`
    FilteredMappedListRef {
        /// The source HOLD ID containing the list
        source_hold: Arc<str>,
        /// The field to filter on (e.g., "completed")
        filter_field: Arc<str>,
        /// The value to match (e.g., Bool(true) or a WhileRef for dynamic filtering)
        filter_value: Box<DdValue>,
        /// Template element with Placeholder where the item value goes
        element_template: Arc<DdValue>,
    },
    /// Filtered and mapped list with a predicate template - generic filtering.
    /// Created when: `list |> List/retain(item, if: complex_predicate) |> List/map(item, new: element_expr)`
    /// The predicate_template contains Placeholder markers that get substituted with each item.
    /// This allows complex predicates like `selected_filter |> WHILE { All => True, Active => item.completed |> Bool/not() }`
    FilteredMappedListWithPredicate {
        /// The source HOLD ID containing the list (or empty if from concrete list)
        source_hold: Arc<str>,
        /// The predicate template - contains Placeholder markers for item fields.
        /// At render time, for each item, substitute_placeholder is called and the result
        /// is evaluated for truthiness.
        predicate_template: Box<DdValue>,
        /// Template element with Placeholder where the item value goes
        element_template: Arc<DdValue>,
    },
    /// Deferred WHILE pattern match on a PlaceholderField.
    /// Created when: `todo.editing |> WHILE { True => ..., False => ... }` during template evaluation.
    /// At render time (during substitute_placeholder), the field path is resolved against the
    /// actual item value, and this becomes a proper WhileRef.
    PlaceholderWhileRef {
        /// Field access path from the placeholder (e.g., ["editing"])
        field_path: Arc<Vec<Arc<str>>>,
        /// Pre-evaluated arms: (pattern_value, body_result)
        arms: Arc<Vec<(DdValue, DdValue)>>,
        /// Default arm result (for wildcard pattern)
        default: Option<Arc<DdValue>>,
    },
    /// Negated placeholder field - for `item.field |> Bool/not()` in templates.
    /// Created when Bool/not() is applied to a PlaceholderField.
    /// At render time, the field is resolved and the boolean result is negated.
    NegatedPlaceholderField {
        /// Field access path from the placeholder (e.g., ["completed"])
        path: Arc<Vec<Arc<str>>>,
    },
    /// Merged stream reference - LATEST { input1, input2, ... }
    /// Created when LATEST combines multiple reactive inputs.
    /// Downstream operators like Math/sum() recognize this and configure DataflowConfig.
    ///
    /// Example: `LATEST { 0, button.press |> THEN { 1 } } |> Math/sum()`
    /// Creates LatestRef with initial=0, events=[LinkRef("link_1")]
    LatestRef {
        /// Initial/default value (first non-event input, or Unit)
        initial: Box<DdValue>,
        /// Event sources (LinkRefs or TimerRefs that trigger updates)
        events: Arc<Vec<DdValue>>,
        /// Event values (what each event emits when triggered)
        event_values: Arc<Vec<DdValue>>,
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
            // ReactiveText - truthy if it has parts
            Self::ReactiveText { parts } => !parts.is_empty(),
            // Placeholder - always truthy (represents a value slot)
            Self::Placeholder => true,
            // PlaceholderField - always truthy (represents deferred field access)
            Self::PlaceholderField { .. } => true,
            // PlaceholderWhileRef - always truthy (deferred WHILE on placeholder)
            Self::PlaceholderWhileRef { .. } => true,
            // NegatedPlaceholderField - always truthy (deferred negated field access)
            Self::NegatedPlaceholderField { .. } => true,
            // MappedListRef - truthy if source HOLD exists
            Self::MappedListRef { source_hold, .. } => {
                super::io::get_hold_value(source_hold).is_some()
            }
            // FilteredMappedListRef - truthy if source HOLD exists
            Self::FilteredMappedListRef { source_hold, .. } => {
                super::io::get_hold_value(source_hold).is_some()
            }
            // FilteredListRefWithPredicate - truthy if source HOLD exists
            Self::FilteredListRefWithPredicate { source_hold, .. } => {
                super::io::get_hold_value(source_hold).is_some()
            }
            // FilteredMappedListWithPredicate - truthy if source HOLD exists
            Self::FilteredMappedListWithPredicate { source_hold, .. } => {
                source_hold.is_empty() || super::io::get_hold_value(source_hold).is_some()
            }
            // LatestRef - truthy if initial or any events exist
            Self::LatestRef { initial, events, .. } => {
                initial.is_truthy() || !events.is_empty()
            }
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
            Self::HoldRef(name) => {
                // Resolve HoldRef to actual HOLD value for display
                super::io::get_hold_value(name)
                    .map(|v| v.to_display_string())
                    .unwrap_or_else(|| String::new())
            }
            Self::LinkRef(path) => format!("[link:{}]", path),
            Self::TimerRef { id, interval_ms } => format!("[timer:{}@{}ms]", id, interval_ms),
            Self::WhileRef { hold_id, computation, arms, default } => {
                // Try to evaluate the WhileRef with current HOLD state
                if let Some(hold_value) = super::io::get_hold_value(hold_id) {
                    // If there's a computation, evaluate it first
                    let match_value = if let Some(comp) = computation {
                        evaluate_computed(comp, &hold_value)
                    } else {
                        hold_value
                    };

                    // Try to match against arms
                    for (pattern, body) in arms.iter() {
                        if &match_value == pattern {
                            return body.to_display_string();
                        }
                    }

                    // No match - use default
                    if let Some(def) = default {
                        return def.to_display_string();
                    }
                }
                // HOLD not available or no match - return empty
                String::new()
            }
            Self::ComputedRef { computation, source_hold } => {
                // Try to evaluate the computation with current HOLD state
                if let Some(source_value) = super::io::get_hold_value(source_hold) {
                    evaluate_computed(computation, &source_value).to_display_string()
                } else {
                    // HOLD not available - return empty (bridge will handle reactively)
                    String::new()
                }
            }
            Self::FilteredListRef { source_hold, filter_field, .. } => {
                format!("[filtered:{}@{}]", filter_field, source_hold)
            }
            Self::ReactiveFilteredList { items, filter_field, .. } => {
                format!("[reactive-filtered:{}#{}]", filter_field, items.len())
            }
            Self::ReactiveText { parts } => {
                // Evaluate all parts with current HOLD state and concatenate
                parts.iter()
                    .map(|part| part.to_display_string())
                    .collect()
            }
            Self::Placeholder => "[placeholder]".to_string(),
            Self::PlaceholderField { path } => {
                format!("[placeholder.{}]", path.join("."))
            }
            Self::PlaceholderWhileRef { field_path, .. } => {
                format!("[placeholder-while.{}]", field_path.join("."))
            }
            Self::NegatedPlaceholderField { path } => {
                format!("[not-placeholder.{}]", path.join("."))
            }
            Self::MappedListRef { source_hold, .. } => {
                format!("[mapped-list:{}]", source_hold)
            }
            Self::FilteredMappedListRef { source_hold, filter_field, .. } => {
                format!("[filtered-mapped-list:{}@{}]", filter_field, source_hold)
            }
            Self::FilteredListRefWithPredicate { source_hold, .. } => {
                format!("[filtered-list-predicate:{}]", source_hold)
            }
            Self::FilteredMappedListWithPredicate { source_hold, .. } => {
                format!("[filtered-mapped-list-predicate:{}]", source_hold)
            }
            Self::LatestRef { initial, .. } => {
                // In text context, LATEST displays its initial/current value
                initial.to_display_string()
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

    /// Create a mapped list reference.
    pub fn mapped_list_ref(source_hold: impl Into<Arc<str>>, element_template: DdValue) -> Self {
        Self::MappedListRef {
            source_hold: source_hold.into(),
            element_template: Arc::new(element_template),
        }
    }

    /// Substitute all Placeholder occurrences with the given value.
    /// Used by MappedListRef to create concrete elements from templates.
    pub fn substitute_placeholder(&self, value: &DdValue) -> DdValue {
        match self {
            Self::Placeholder => value.clone(),
            // PlaceholderField - resolve the path against the substituted value
            Self::PlaceholderField { path } => {
                let mut current = value.clone();
                for field in path.iter() {
                    current = current.get(field.as_ref()).cloned().unwrap_or(DdValue::Unit);
                }
                current
            }
            // PlaceholderWhileRef - resolve field path and create WhileRef
            Self::PlaceholderWhileRef { field_path, arms, default } => {
                // Resolve the field path against the actual item value
                let mut resolved = value.clone();
                for field in field_path.iter() {
                    resolved = resolved.get(field.as_ref()).cloned().unwrap_or(DdValue::Unit);
                }

                // Substitute placeholders in the arms too
                let new_arms: Vec<(DdValue, DdValue)> = arms
                    .iter()
                    .map(|(pattern, body)| {
                        (pattern.substitute_placeholder(value), body.substitute_placeholder(value))
                    })
                    .collect();

                let new_default = default.as_ref().map(|d| Arc::new(d.substitute_placeholder(value)));

                // If resolved to HoldRef, create a proper WhileRef
                if let DdValue::HoldRef(hold_id) = resolved {
                    Self::WhileRef {
                        hold_id,
                        computation: None,
                        arms: Arc::new(new_arms),
                        default: new_default,
                    }
                } else {
                    // For non-HoldRef values, try to match synchronously
                    for (pattern, body) in new_arms.iter() {
                        if &resolved == pattern {
                            return body.clone();
                        }
                    }
                    // No match - use default or Unit
                    new_default.map(|d| (*d).clone()).unwrap_or(DdValue::Unit)
                }
            }
            // NegatedPlaceholderField - resolve field path and negate the result
            Self::NegatedPlaceholderField { path } => {
                // Resolve the field path against the actual item value
                let mut resolved = value.clone();
                for field in path.iter() {
                    resolved = resolved.get(field.as_ref()).cloned().unwrap_or(DdValue::Unit);
                }
                // If resolved to a HoldRef, we need to keep it reactive
                // but since negation is a simple operation, we can check for Bool values
                match resolved {
                    DdValue::Bool(b) => DdValue::Bool(!b),
                    DdValue::Tagged { ref tag, .. } => {
                        // True -> False, False -> True
                        if tag.as_ref() == "True" {
                            DdValue::Bool(false)
                        } else if tag.as_ref() == "False" {
                            DdValue::Bool(true)
                        } else {
                            DdValue::Bool(!resolved.is_truthy())
                        }
                    }
                    DdValue::HoldRef(hold_id) => {
                        // For HoldRef, look up and negate
                        let hold_value = super::io::get_hold_value(&hold_id)
                            .unwrap_or(DdValue::Bool(false));
                        DdValue::Bool(!hold_value.is_truthy())
                    }
                    _ => DdValue::Bool(!resolved.is_truthy()),
                }
            }
            Self::Object(fields) => {
                let new_fields: BTreeMap<Arc<str>, DdValue> = fields
                    .iter()
                    .map(|(k, v)| (k.clone(), v.substitute_placeholder(value)))
                    .collect();
                Self::Object(Arc::new(new_fields))
            }
            Self::List(items) => {
                let new_items: Vec<DdValue> = items
                    .iter()
                    .map(|item| item.substitute_placeholder(value))
                    .collect();
                Self::List(Arc::new(new_items))
            }
            Self::Tagged { tag, fields } => {
                let new_fields: BTreeMap<Arc<str>, DdValue> = fields
                    .iter()
                    .map(|(k, v)| (k.clone(), v.substitute_placeholder(value)))
                    .collect();
                Self::Tagged {
                    tag: tag.clone(),
                    fields: Arc::new(new_fields),
                }
            }
            Self::ReactiveText { parts } => {
                let new_parts: Vec<DdValue> = parts
                    .iter()
                    .map(|part| part.substitute_placeholder(value))
                    .collect();
                Self::ReactiveText { parts: Arc::new(new_parts) }
            }
            // WhileRef - substitute placeholders in arm bodies
            Self::WhileRef { hold_id, computation, arms, default } => {
                let new_arms: Vec<(DdValue, DdValue)> = arms
                    .iter()
                    .map(|(pattern, body)| {
                        (pattern.clone(), body.substitute_placeholder(value))
                    })
                    .collect();
                let new_default = default.as_ref().map(|d| Arc::new(d.substitute_placeholder(value)));
                Self::WhileRef {
                    hold_id: hold_id.clone(),
                    computation: computation.clone(),
                    arms: Arc::new(new_arms),
                    default: new_default,
                }
            }
            // For all other types, no substitution needed
            _ => self.clone(),
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
                                // Resolve HoldRef before comparing (items may have completed: HoldRef("hold_11"))
                                match obj.get(field.as_ref()) {
                                    Some(DdValue::HoldRef(hold_id)) => {
                                        let hold_value = super::io::get_hold_value(hold_id)
                                            .unwrap_or(DdValue::Unit);
                                        &hold_value == value.as_ref()
                                    }
                                    Some(field_value) => field_value == value.as_ref(),
                                    None => false,
                                }
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
                                // Resolve HoldRef before comparing
                                match obj.get(field.as_ref()) {
                                    Some(DdValue::HoldRef(hold_id)) => {
                                        let hold_value = super::io::get_hold_value(hold_id)
                                            .unwrap_or(DdValue::Unit);
                                        &hold_value == value.as_ref()
                                    }
                                    Some(field_value) => field_value == value.as_ref(),
                                    None => false,
                                }
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
        // DELETED: ComputedType::ReactiveListCountWhere match arm - was dead code (never constructed)
        ComputedType::ListCountWhereHold { source_hold, field, value } => {
            // Get LIVE items from source HOLD (includes dynamic items!)
            let items = super::io::get_hold_value(source_hold.as_ref())
                .and_then(|v| match v {
                    DdValue::List(list) => Some(list),
                    _ => None,
                })
                .unwrap_or_default();

            // Count items where field matches value
            let count = items.iter()
                .filter(|item| {
                    if let DdValue::Object(obj) = item {
                        match obj.get(field.as_ref()) {
                            // Resolve HoldRef to get current value
                            Some(DdValue::HoldRef(hold_id)) => {
                                let hold_value = super::io::get_hold_value(hold_id)
                                    .unwrap_or(DdValue::Unit);
                                &hold_value == value.as_ref()
                            }
                            // Direct value comparison
                            Some(field_value) => field_value == value.as_ref(),
                            None => false,
                        }
                    } else {
                        false
                    }
                })
                .count();
            zoon::println!("[DD ListCountWhereHold] source={}, count={}", source_hold, count);
            DdValue::int(count as i64)
        }
        ComputedType::ListCountHold { source_hold } => {
            // Get LIVE items from source HOLD (includes dynamic items!)
            let items = super::io::get_hold_value(source_hold.as_ref())
                .and_then(|v| match v {
                    DdValue::List(list) => Some(list),
                    _ => None,
                })
                .unwrap_or_default();
            let count = items.len();
            zoon::println!("[DD ListCountHold] source={}, count={}", source_hold, count);
            DdValue::int(count as i64)
        }
        ComputedType::ListIsEmptyHold { source_hold } => {
            // Get LIVE items from source HOLD (includes dynamic items!)
            let items = super::io::get_hold_value(source_hold.as_ref())
                .and_then(|v| match v {
                    DdValue::List(list) => Some(list),
                    _ => None,
                })
                .unwrap_or_default();
            let is_empty = items.is_empty();
            zoon::println!("[DD ListIsEmptyHold] source={}, is_empty={}", source_hold, is_empty);
            DdValue::Bool(is_empty)
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
