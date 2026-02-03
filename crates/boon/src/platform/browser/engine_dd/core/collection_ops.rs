//! Collection operation definitions for DD dataflow.

use std::sync::Arc;

use super::value::{CollectionId, TemplateValue, Value};

/// Type of collection operation.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CollectionOp {
    /// Filter items by predicate.
    /// Replaces: FilteredListRef, FilteredListRefWithPredicate, ReactiveFilteredList
    Filter {
        /// For simple field equality: Some((field_name, expected_value))
        /// For complex predicates: None (use predicate_template)
        field_filter: Option<(Arc<str>, Value)>,
        /// For complex predicates: evaluated template with Placeholder
        predicate_template: Option<TemplateValue>,
    },
    /// Map/transform items.
    /// Replaces: MappedListRef, FilteredMappedListRef
    Map {
        /// Element template with Placeholder for item substitution
        element_template: TemplateValue,
    },
    /// Count items.
    /// Replaces: ComputedRef::ListCount
    Count,
    /// Count items matching filter.
    /// Replaces: ComputedRef::ListCountWhere, ComputedRef::ListCountWhereHold
    CountWhere {
        /// Field to filter on
        filter_field: Arc<str>,
        /// Value to match
        filter_value: Value,
    },
    /// Check if list is empty.
    IsEmpty,
    /// Concatenate two collections.
    Concat {
        /// Second collection to concatenate
        other_source: CollectionId,
    },
    // ══════════════════════════════════════════════════════════════════════════
    // Phase 4: Arithmetic/Comparison Operations
    // Replaces: ComputedRef::Subtract, ComputedRef::GreaterThanZero, ComputedRef::Equal
    // These operate on Count/CountWhere outputs to produce derived values.
    // ══════════════════════════════════════════════════════════════════════════
    /// Subtract one count from another (produces Number).
    /// Used for: active_list_count = list_count - completed_list_count
    Subtract {
        /// Second operand (subtracted from source)
        right_source: CollectionId,
    },
    /// Check if a count is greater than zero (produces Bool).
    /// Used for: show_clear_completed = completed_list_count > 0
    GreaterThanZero,
    /// Compare two values for equality (produces Bool).
    /// Used for: all_completed = completed_list_count == list_count
    Equal {
        /// Second operand to compare against source
        right_source: CollectionId,
    },
}

/// Configuration for a collection operation in the DD dataflow.
///
/// Each operation takes one (or more) input collections and produces
/// an output collection.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CollectionOpConfig {
    /// Unique ID for the output collection
    pub output_id: CollectionId,
    /// Source collection ID (input to this operation)
    pub source_id: CollectionId,
    /// The operation to perform
    pub op: CollectionOp,
}
