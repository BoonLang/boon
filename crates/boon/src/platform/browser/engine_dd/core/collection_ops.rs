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
    // Arithmetic/Comparison Operations
    // These operate on Count/CountWhere outputs to produce derived values.
    // ══════════════════════════════════════════════════════════════════════════
    /// Subtract one count from another (produces Number).
    Subtract {
        /// Second operand (subtracted from source)
        right_source: CollectionId,
    },
    /// Check if a count is greater than zero (produces Bool).
    GreaterThanZero,
    /// Compare two values for equality (produces Bool).
    Equal {
        /// Second operand to compare against source
        right_source: CollectionId,
    },
    // ══════════════════════════════════════════════════════════════════════════
    // Scalar Transform Operations
    // These transform scalar cell values through pattern matching or formatting.
    // ══════════════════════════════════════════════════════════════════════════
    /// Pattern-match a scalar cell value to produce a derived scalar.
    /// Used for: count |> WHEN { 1 => "", __ => "s" } in reactive text.
    ScalarWhen {
        /// Pattern-value pairs: (pattern_to_match, output_value)
        arms: Vec<(Value, Value)>,
        /// Default output when no pattern matches
        default: Value,
    },
    /// Format text from multiple reactive cell sources.
    /// Used for: TEXT { {count} item{maybe_s} left } with reactive interpolation.
    ComputedText {
        /// Template parts describing the output format
        parts: Vec<ComputedTextPart>,
        /// Additional cell sources beyond source_id (index 1+)
        extra_sources: Vec<CollectionId>,
    },
}

/// A part of a computed text template.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ComputedTextPart {
    /// Static text segment
    Static(Arc<str>),
    /// Reference to a cell source by index (0 = source_id, 1+ = extra_sources)
    CellSource(usize),
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
