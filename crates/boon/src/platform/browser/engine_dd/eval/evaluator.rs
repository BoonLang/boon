//! DD-based evaluator for Boon.
//!
//! This module evaluates Boon AST using simple value types.
//! This is phase 1: static evaluation without reactive features.
//! Phase 2 will add DD-based reactive evaluation.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

#[allow(unused_imports)]
use super::super::dd_log;
use super::super::core::value::{
    attach_item_key, CollectionHandle, CollectionId, PlaceholderWhileConfig, TemplateValue, Value, WhileArm,
    WhileConfig,
};
use super::super::core::{
    DataflowConfig, CellConfig, CellId, LinkId, EventFilter, StateTransform, BoolTag,
    Key, ListAppendBinding, ITEM_KEY_FIELD,
    ListItemTemplate, FieldInitializer, ItemIdentitySpec, get_link_ref_at_path,
};
use super::super::core::types::{LinkAction, LinkCellMapping};
use crate::parser::{Persistence, PersistenceId};
use crate::parser::static_expression::{
    Alias, Arm, ArithmeticOperator, Comparator, Expression, Literal, Object, Pattern, Spanned, TextPart,
};

/// Extract milliseconds from a Duration value or bare Number.
/// Panics if the value is not a valid duration.
fn duration_to_millis(value: &Value) -> u64 {
    match value {
        Value::Tagged { tag, fields } if tag.as_ref() == "Duration" => {
            if let Some(Value::Number(secs)) = fields.get("seconds") {
                let ms = secs.0 * 1000.0;
                u64::try_from(ms as i64).unwrap_or_else(|_| {
                    panic!("[DD_EVAL] Duration seconds {} out of u64 range", secs.0)
                })
            } else if let Some(Value::Number(ms)) = fields.get("millis") {
                u64::try_from(ms.0 as i64).unwrap_or_else(|_| {
                    panic!("[DD_EVAL] Duration millis {} out of u64 range", ms.0)
                })
            } else {
                panic!("[DD_EVAL] Duration missing seconds/millis fields")
            }
        }
        Value::Number(ms) => {
            u64::try_from(ms.0 as i64).unwrap_or_else(|_| {
                panic!("[DD_EVAL] Timer interval {} out of u64 range", ms.0)
            })
        }
        other => panic!("[DD_EVAL] Timer/interval expects Duration or Number, found {:?}", other),
    }
}

/// A stored function definition.
#[derive(Clone)]
struct FunctionDef {
    parameters: Vec<String>,
    body: Box<Spanned<Expression>>,
}

struct BoolLinkBinding {
    link_id: String,
    action: LinkAction,
    key_filter: Option<Vec<Key>>,
}

#[derive(Clone)]
struct LatestLinkBinding {
    link_id: String,
    action: LinkAction,
    key_filter: Option<Vec<Key>>,
}

struct LatestCollectionInput {
    id: CollectionId,
    cell_id: Option<Arc<str>>,
}

/// The DD-based Boon runtime.
pub struct BoonDdRuntime {
    /// Variable values
    variables: HashMap<String, Value>,
    /// Function definitions
    functions: HashMap<String, FunctionDef>,
    /// PASSED context for function calls
    passed_context: Option<Value>,
    /// Current context path for LINK naming (e.g., "increment_button.event.press")
    context_path: Vec<String>,
    /// DataflowConfig built during evaluation (Task 4.4: declarative config builder)
    dataflow_config: DataflowConfig,
    /// Mapping from CellId (HOLD name) to CollectionId for lists
    /// When a HOLD contains a list, we register it here so List/retain, List/map
    /// can look up the source CollectionId for DD operations.
    cell_to_collection: HashMap<String, super::super::core::value::CollectionId>,
    /// Track LATEST event bindings by cell ID (for Math/sum on event streams)
    latest_cells: HashMap<String, Vec<LatestLinkBinding>>,
}

impl BoonDdRuntime {
    /// Create a new DD runtime.
    pub fn new() -> Self {
        Self {
            variables: HashMap::new(),
            functions: HashMap::new(),
            passed_context: None,
            context_path: Vec::new(),
            dataflow_config: DataflowConfig::new(),
            cell_to_collection: HashMap::new(),
            latest_cells: HashMap::new(),
        }
    }

    /// Create a forked runtime for evaluating sub-expressions.
    /// Clones variables, functions, passed_context, context_path, and latest_cells
    /// but starts fresh with empty dataflow_config and cell_to_collection.
    fn fork(&self) -> Self {
        Self {
            variables: self.variables.clone(),
            functions: self.functions.clone(),
            passed_context: self.passed_context.clone(),
            context_path: self.context_path.clone(),
            dataflow_config: DataflowConfig::new(),
            cell_to_collection: self.cell_to_collection.clone(),
            latest_cells: self.latest_cells.clone(),
        }
    }

    /// Fork, evaluate an expression, and merge the fork's dataflow_config and
    /// cell_to_collection back into self. This ensures collections, cells, and
    /// link mappings created during forked evaluation (e.g., WHILE arm bodies)
    /// are not lost.
    fn fork_eval_merge(
        &mut self,
        setup: impl FnOnce(&mut Self),
        expr: &Expression,
    ) -> Value {
        let mut forked = self.fork();
        setup(&mut forked);
        let result = forked.eval_expression(expr);
        let n_cells = forked.dataflow_config.cells.len();
        let n_colls = forked.dataflow_config.initial_collections.len();
        let n_ops = forked.dataflow_config.collection_ops.len();
        let n_links = forked.dataflow_config.link_mappings.len();
        let n_c2c = forked.cell_to_collection.len();
        if n_cells > 0 || n_colls > 0 || n_ops > 0 || n_links > 0 || n_c2c > 0 {
            dd_log!("[DD_EVAL] fork_eval_merge: merging {} cells, {} collections, {} ops, {} links, {} c2c",
                n_cells, n_colls, n_ops, n_links, n_c2c);
        }
        self.dataflow_config.merge_from(forked.dataflow_config);
        self.cell_to_collection.extend(forked.cell_to_collection);
        result
    }

    /// Build a synthetic event object for a LINK reference.
    /// Maps event types to the same LinkRef, with sub-objects for events that carry typed payloads.
    /// - `press`, `click`, `blur`, `double_click` → LinkRef directly (no sub-fields)
    /// - `key_down` → Object { key: LinkRef, text: LinkRef } (key_down events carry key + optional text)
    /// - `change` → Object { text: LinkRef } (change events carry text)
    fn build_link_event_object(link_id: &LinkId) -> Value {
        let link_ref = Value::LinkRef(link_id.clone());
        Value::object([
            ("press", link_ref.clone()),
            ("click", link_ref.clone()),
            ("blur", link_ref.clone()),
            ("key_down", Value::object([
                ("key", link_ref.clone()),
                ("text", link_ref.clone()),
            ])),
            ("double_click", link_ref.clone()),
            ("change", Value::object([
                ("text", link_ref),
            ])),
        ])
    }

    /// Take the built DataflowConfig, leaving an empty one in its place.
    /// Called by dd_interpreter.rs after evaluation to get the config.
    pub fn take_config(&mut self) -> DataflowConfig {
        std::mem::take(&mut self.dataflow_config)
    }

    /// Get a reference to the current config (for debugging).
    pub fn config(&self) -> &DataflowConfig {
        &self.dataflow_config
    }

    /// Add a CellConfig entry during evaluation.
    /// This is called from eval_hold when a HOLD is encountered.
    fn add_cell_config(&mut self, config: CellConfig) {
        dd_log!("[DD_EVAL] Adding CellConfig: id={}, transform={:?}, triggers={:?}",
            config.id.name(), config.transform, config.triggered_by.iter().map(|l| l.name()).collect::<Vec<_>>());
        self.dataflow_config.cells.push(config);
    }

    // ══════════════════════════════════════════════════════════════════════════════
    // Collection operation helpers
    //
    // These methods replace the symbolic reference types
    // (FilteredListRef, MappedListRef, ComputedRef, etc.) with DD-native patterns.
    // ══════════════════════════════════════════════════════════════════════════════

    /// Register a list HOLD and get its CollectionId.
    /// Called when evaluating a HOLD that contains a list.
    fn register_list_hold(
        &mut self,
        cell_id: &str,
        items: Vec<Value>,
    ) -> super::super::core::value::CollectionId {
        dd_log!("[DD_EVAL] register_list_hold: cell_id={}, {} items", cell_id, items.len());
        for (i, item) in items.iter().enumerate() {
            dd_log!("[DD_EVAL]   hold_item[{}] = {:?}", i, item);
        }
        if !items.is_empty() {
            let mut seen: HashSet<Arc<str>> = HashSet::new();
            for item in &items {
                let fields = match item {
                    Value::Object(fields) => fields,
                    Value::Tagged { fields, .. } => fields,
                    other => {
                        panic!("[DD_EVAL] list item must be Object/Tagged, found {:?}", other);
                    }
                };
                let key = fields.get(ITEM_KEY_FIELD).unwrap_or_else(|| {
                    panic!("[DD_EVAL] list item missing '{}'", ITEM_KEY_FIELD);
                });
                let key = match key {
                    Value::Text(key) => key.clone(),
                    other => {
                        panic!("[DD_EVAL] list item '{}' must be Text, found {:?}", ITEM_KEY_FIELD, other);
                    }
                };
                if !seen.insert(key.clone()) {
                    panic!("[DD_EVAL] duplicate '{}' value '{}' in list '{}'", ITEM_KEY_FIELD, key, cell_id);
                }
            }
        }
        if let Some(existing) = self.get_collection_id(cell_id) {
            if !items.is_empty() {
                if let Some(existing_items) = self.dataflow_config.initial_collections.get_mut(&existing) {
                    if existing_items.is_empty() {
                        *existing_items = items.clone();
                    } else if *existing_items != items {
                        panic!(
                            "[DD_EVAL] Conflicting initial list items for '{}': {:?} vs {:?}",
                            cell_id, existing_items, items
                        );
                    }
                } else if self.dataflow_config.initial_collections.contains_key(&existing) {
                    // Already registered, skip
                } else {
                    // Only register source if this collection is local (not inherited from parent fork)
                    self.dataflow_config.initial_collections.insert(existing.clone(), items.clone());
                }
            }
            // Only register source if this collection is in local initial_collections
            // (inherited collections from parent fork don't need re-registration)
            if self.dataflow_config.initial_collections.contains_key(&existing) {
                self.dataflow_config.add_collection_source(existing.clone(), cell_id.to_string());
            }
            return existing;
        }

        let collection_id = self.dataflow_config.add_initial_collection(items);
        self.dataflow_config.add_collection_source(collection_id.clone(), cell_id.to_string());
        self.cell_to_collection.insert(cell_id.to_string(), collection_id.clone());
        dd_log!("[DD_EVAL] Registered list HOLD '{}' as CollectionId({:?})", cell_id, collection_id);
        collection_id
    }

    /// Register a list literal as a DD collection with auto-generated __key values.
    fn register_list_literal_collection(&mut self, items: Vec<Value>) -> CollectionId {
        let collection_id = CollectionId::new();
        let key_prefix = collection_id.to_string();
        dd_log!("[DD_EVAL] register_list_literal_collection: {} items, prefix={}", items.len(), key_prefix);
        for (i, item) in items.iter().enumerate() {
            dd_log!("[DD_EVAL]   item[{}] = {:?}", i, item);
        }
        let items_with_keys = self.attach_auto_keys_to_list_literal(items, &key_prefix);
        self.dataflow_config
            .add_initial_collection_with_id(collection_id, items_with_keys);
        collection_id
    }

    fn attach_auto_keys_to_list_literal(&self, items: Vec<Value>, key_prefix: &str) -> Vec<Value> {
        items
            .into_iter()
            .enumerate()
            .map(|(idx, item)| self.attach_auto_key_to_item(item, key_prefix, idx))
            .collect()
    }

    fn attach_auto_key_to_item(&self, item: Value, key_prefix: &str, index: usize) -> Value {
        match &item {
            Value::Object(fields) | Value::Tagged { fields, .. } => {
                if fields.contains_key(ITEM_KEY_FIELD) {
                    panic!(
                        "[DD_EVAL] '{}' is internal; list items must not define it",
                        ITEM_KEY_FIELD
                    );
                }
            }
            _ => {
                // Non-Object/Tagged items (e.g., Text in font-family lists)
                // don't support __key fields — return as-is.
                return item;
            }
        }
        let key = format!("{}#{}", key_prefix, index);
        attach_item_key(item, &key)
    }

    /// Get the CollectionId for a HOLD cell, if it contains a list.
    fn get_collection_id(&self, cell_id: &str) -> Option<super::super::core::value::CollectionId> {
        self.cell_to_collection.get(cell_id).cloned()
    }

    fn extract_latest_collection_input(&mut self, value: &Value) -> Option<LatestCollectionInput> {
        match value {
            Value::List(handle) => Some(LatestCollectionInput {
                id: handle.id,
                cell_id: handle.cell_id.clone(),
            }),
            Value::CellRef(cell_id) => {
                let cell_name = cell_id.name();
                let collection_id = self.get_collection_id(&cell_name)?;
                Some(LatestCollectionInput {
                    id: collection_id,
                    cell_id: Some(Arc::from(cell_name)),
                })
            }
            _ => None,
        }
    }

    /// Wrap a CollectionId as a Value::List and register it in cell_to_collection
    /// so it can be resolved later from CellRef names (e.g., in Subtract).
    fn collection_value(&mut self, id: CollectionId) -> Value {
        self.cell_to_collection.insert(id.name(), id);
        Value::List(CollectionHandle::new_with_id(id))
    }

    /// Create a filtered collection (replaces FilteredListRef).
    ///
    /// OLD: Value::FilteredListRef { source_hold, filter_field, filter_value }
    /// NEW: Register filter op in DataflowConfig, return Value::List
    fn create_filtered_collection(
        &mut self,
        source_cell_id: &str,
        filter_field: std::sync::Arc<str>,
        filter_value: Value,
    ) -> Value {
        use super::super::core::value::CollectionHandle;

        let source_id = self
            .get_collection_id(source_cell_id)
            .unwrap_or_else(|| self.register_list_hold(source_cell_id, Vec::new()));
        let output_id = self.dataflow_config.add_filter(
            source_id,
            Some((filter_field, filter_value)),
            None,
        );
        dd_log!("[DD_EVAL] Created filtered collection from '{}' -> CollectionId({:?})",
            source_cell_id, output_id);
        self.collection_value(output_id)
    }

    /// Create a mapped collection (replaces MappedListRef).
    ///
    /// OLD: Value::MappedListRef { source_hold, element_template }
    /// NEW: Register map op in DataflowConfig, return Value::List
    fn create_mapped_collection(
        &mut self,
        source_cell_id: &str,
        element_template: TemplateValue,
    ) -> Value {
        use super::super::core::value::CollectionHandle;

        let source_id = self
            .get_collection_id(source_cell_id)
            .unwrap_or_else(|| self.register_list_hold(source_cell_id, Vec::new()));
        self.dataflow_config.set_list_element_template(source_cell_id, element_template.clone());
        let output_id = self.dataflow_config.add_map(source_id, element_template);
        dd_log!("[DD_EVAL] Created mapped collection from '{}' -> CollectionId({:?})",
            source_cell_id, output_id);
        self.collection_value(output_id)
    }

    /// Create a list count (replaces ComputedRef::ListCount).
    ///
    /// OLD: Value::ComputedRef { computation: ListCount, source_hold }
    /// NEW: Register count op in DataflowConfig, return Value::CellRef
    fn create_list_count(&mut self, source_cell_id: &str) -> Value {
        let source_id = self
            .get_collection_id(source_cell_id)
            .unwrap_or_else(|| self.register_list_hold(source_cell_id, Vec::new()));
        let output_id = self.dataflow_config.add_count(source_id);
        dd_log!(
            "[DD_EVAL] Created list count from '{}' -> cell '{}'",
            source_cell_id,
            output_id.name()
        );
        self.cell_to_collection.insert(output_id.name(), output_id);
        Value::CellRef(super::super::core::types::CellId::new(&output_id.name()))
    }

    /// Create a count-where (replaces ComputedRef::ListCountWhere).
    ///
    /// OLD: Value::ComputedRef { computation: ListCountWhere { filter_field, filter_value }, source_hold }
    /// NEW: Register count-where op in DataflowConfig, return Value::CellRef
    fn create_list_count_where(
        &mut self,
        source_cell_id: &str,
        filter_field: std::sync::Arc<str>,
        filter_value: Value,
    ) -> Value {
        let source_id = self
            .get_collection_id(source_cell_id)
            .unwrap_or_else(|| self.register_list_hold(source_cell_id, Vec::new()));
        let output_id = self.dataflow_config.add_count_where(source_id, filter_field, filter_value);
        dd_log!(
            "[DD_EVAL] Created list count-where from '{}' -> cell '{}'",
            source_cell_id,
            output_id.name()
        );
        self.cell_to_collection.insert(output_id.name(), output_id);
        Value::CellRef(super::super::core::types::CellId::new(&output_id.name()))
    }

    /// Chain a map operation on an existing collection (for filter+map chains).
    ///
    /// OLD: FilteredListRef |> List/map() -> FilteredMappedListRef
    /// NEW: Collection(filtered_id) |> List/map() -> Collection(mapped_id)
    fn chain_map_on_collection(
        &mut self,
        source_id: super::super::core::value::CollectionId,
        element_template: TemplateValue,
    ) -> Value {
        use super::super::core::value::CollectionHandle;

        let output_id = self.dataflow_config.add_map(source_id.clone(), element_template);
        dd_log!("[DD_EVAL] Chained map on CollectionId({:?}) -> CollectionId({:?})",
            source_id, output_id);
        self.collection_value(output_id)
    }

    /// Chain a filter operation on an existing collection (for chained filters).
    fn chain_filter_on_collection(
        &mut self,
        source_id: super::super::core::value::CollectionId,
        filter_field: Option<(std::sync::Arc<str>, Value)>,
        predicate_template: Option<TemplateValue>,
    ) -> Value {
        use super::super::core::value::CollectionHandle;

        let output_id = self.dataflow_config.add_filter(source_id.clone(), filter_field, predicate_template);
        dd_log!("[DD_EVAL] Chained filter on CollectionId({:?}) -> CollectionId({:?})",
            source_id, output_id);
        self.collection_value(output_id)
    }

    /// Create a list is_empty check (replaces ComputedRef::ListIsEmpty).
    fn create_list_is_empty(&mut self, source_cell_id: &str) -> Value {
        let source_id = self
            .get_collection_id(source_cell_id)
            .unwrap_or_else(|| self.register_list_hold(source_cell_id, Vec::new()));
        let output_id = self.dataflow_config.add_is_empty(source_id);
        dd_log!(
            "[DD_EVAL] Created list is_empty from '{}' -> cell '{}'",
            source_cell_id,
            output_id.name()
        );
        self.cell_to_collection.insert(output_id.name(), output_id);
        Value::CellRef(super::super::core::types::CellId::new(&output_id.name()))
    }

    /// Chain a count operation on an existing collection.
    fn chain_count_on_collection(
        &mut self,
        source_id: super::super::core::value::CollectionId,
    ) -> Value {
        let output_id = self.dataflow_config.add_count(source_id.clone());
        dd_log!(
            "[DD_EVAL] Chained count on CollectionId({:?}) -> cell '{}'",
            source_id,
            output_id.name()
        );
        self.cell_to_collection.insert(output_id.name(), output_id);
        Value::CellRef(super::super::core::types::CellId::new(&output_id.name()))
    }

    /// Chain an is_empty operation on an existing collection.
    fn chain_is_empty_on_collection(
        &mut self,
        source_id: super::super::core::value::CollectionId,
    ) -> Value {
        let output_id = self.dataflow_config.add_is_empty(source_id.clone());
        dd_log!(
            "[DD_EVAL] Chained is_empty on CollectionId({:?}) -> cell '{}'",
            source_id,
            output_id.name()
        );
        self.cell_to_collection.insert(output_id.name(), output_id);
        Value::CellRef(super::super::core::types::CellId::new(&output_id.name()))
    }

    fn chain_concat_on_collection(
        &mut self,
        left: super::super::core::value::CollectionId,
        right: super::super::core::value::CollectionId,
    ) -> super::super::core::value::CollectionId {
        let output_id = self.dataflow_config.add_concat(left, right);
        dd_log!(
            "[DD_EVAL] Chained concat -> CollectionId({:?})",
            output_id
        );
        output_id
    }

    // ══════════════════════════════════════════════════════════════════════════════
    // Arithmetic/Comparison Operation Helpers
    // ══════════════════════════════════════════════════════════════════════════════

    /// Create a subtract operation (left - right).
    /// Replaces: ComputedRef::Subtract
    /// Used for: active_list_count = list_count - completed_list_count
    fn create_subtract(
        &mut self,
        left: super::super::core::value::CollectionId,
        right: super::super::core::value::CollectionId,
    ) -> Value {
        let output_id = self.dataflow_config.add_subtract(left.clone(), right.clone());
        dd_log!(
            "[DD_EVAL] Created subtract: CollectionId({:?}) - CollectionId({:?}) -> cell '{}'",
            left,
            right,
            output_id.name()
        );
        self.cell_to_collection.insert(output_id.name(), output_id);
        Value::CellRef(super::super::core::types::CellId::new(&output_id.name()))
    }

    /// Create a greater-than-zero check.
    /// Replaces: ComputedRef::GreaterThanZero
    /// Used for: show_clear_completed = completed_list_count > 0
    fn create_greater_than_zero(
        &mut self,
        source_id: super::super::core::value::CollectionId,
    ) -> Value {
        let output_id = self.dataflow_config.add_greater_than_zero(source_id.clone());
        dd_log!(
            "[DD_EVAL] Created greater_than_zero: CollectionId({:?}) > 0 -> cell '{}'",
            source_id,
            output_id.name()
        );
        self.cell_to_collection.insert(output_id.name(), output_id);
        Value::CellRef(super::super::core::types::CellId::new(&output_id.name()))
    }

    /// Create an equality comparison.
    /// Replaces: ComputedRef::Equal
    /// Used for: all_completed = completed_list_count == list_count
    fn create_equal(
        &mut self,
        left: super::super::core::value::CollectionId,
        right: super::super::core::value::CollectionId,
    ) -> Value {
        let output_id = self.dataflow_config.add_equal(left.clone(), right.clone());
        dd_log!(
            "[DD_EVAL] Created equal: CollectionId({:?}) == CollectionId({:?}) -> cell '{}'",
            left,
            right,
            output_id.name()
        );
        self.cell_to_collection.insert(output_id.name(), output_id);
        Value::CellRef(super::super::core::types::CellId::new(&output_id.name()))
    }

    /// Create a scalar pattern-match cell (ScalarWhen).
    /// Used for reactive WHEN on scalar cells (e.g., count |> WHEN { 1 => "", __ => "s" }).
    fn create_scalar_when(
        &mut self,
        source_collection: super::super::core::value::CollectionId,
        arms: Vec<(Value, Value)>,
        default: Value,
    ) -> Value {
        let output_id = self.dataflow_config.add_scalar_when(source_collection.clone(), arms, default);
        dd_log!(
            "[DD_EVAL] Created ScalarWhen: source {:?} -> cell '{}'",
            source_collection,
            output_id.name()
        );
        self.cell_to_collection.insert(output_id.name(), output_id);
        Value::CellRef(super::super::core::types::CellId::new(&output_id.name()))
    }

    /// Create a computed text cell (reactive TEXT interpolation).
    /// Used for TEXT { {count} item{maybe_s} left } with reactive CellRef parts.
    fn create_computed_text(
        &mut self,
        sources: Vec<super::super::core::value::CollectionId>,
        parts: Vec<super::super::core::collection_ops::ComputedTextPart>,
    ) -> Value {
        let output_id = self.dataflow_config.add_computed_text(sources, parts);
        dd_log!(
            "[DD_EVAL] Created ComputedText -> cell '{}'",
            output_id.name()
        );
        self.cell_to_collection.insert(output_id.name(), output_id);
        Value::CellRef(super::super::core::types::CellId::new(&output_id.name()))
    }

    /// Try to get the CollectionId for a CellRef name.
    /// Returns None if the cell is not backed by a collection op.
    fn try_get_collection_id_for_cellref(&self, cell_id: &super::super::core::types::CellId) -> Option<super::super::core::value::CollectionId> {
        self.get_collection_id(&cell_id.name())
    }

    // ══════════════════════════════════════════════════════════════════════════════
    // End helpers
    // ══════════════════════════════════════════════════════════════════════════════

    /// Determine the StateTransform from HOLD body pattern.
    ///
    /// Patterns detected:
    /// - `state |> Bool/not()` → BoolToggle
    /// - `state + 1` → Increment
    /// - `True` or `False` tag → SetTrue/SetFalse
    /// - Default → Identity (no transform, just propagate event)
    fn determine_transform(&self, body: &Expression, state_name: &str) -> StateTransform {
        // Look for patterns in THEN body
        match body {
            Expression::Pipe { from, to } => {
                // Pattern: event |> THEN { transform_body }
                if let Expression::Then { body: then_body } = &to.node {
                    return self.determine_transform_from_then_body(&then_body.node, state_name);
                }
                // Pattern: LATEST { ... } |> something
                if let Expression::Latest { inputs, .. } = &from.node {
                    // Check first input that has THEN
                    for input in inputs {
                        if let Some(transform) = self.determine_transform_from_input(&input.node, state_name) {
                            return transform;
                        }
                    }
                }
                StateTransform::Identity
            }
            Expression::Latest { inputs, .. } => {
                // Check inputs for transform patterns
                for input in inputs {
                    if let Some(transform) = self.determine_transform_from_input(&input.node, state_name) {
                        return transform;
                    }
                }
                StateTransform::Identity
            }
            _ => StateTransform::Identity,
        }
    }

    /// Helper to determine transform from a single LATEST input.
    fn determine_transform_from_input(&self, input: &Expression, state_name: &str) -> Option<StateTransform> {
        match input {
            Expression::Pipe { from: _, to } => {
                if let Expression::Then { body: then_body } = &to.node {
                    let transform = self.determine_transform_from_then_body(&then_body.node, state_name);
                    if !matches!(transform, StateTransform::Identity) {
                        return Some(transform);
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Determine transform from the body of a THEN expression.
    fn determine_transform_from_then_body(&self, body: &Expression, state_name: &str) -> StateTransform {
        match body {
            // Pattern: state |> Bool/not() → BoolToggle
            Expression::Pipe { from, to } => {
                if let Expression::FunctionCall { path, .. } = &to.node {
                    let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                    if path_strs == ["Bool", "not"] {
                        if self.is_bool_toggle_pattern(body, state_name) {
                            return StateTransform::BoolToggle;
                        }
                        panic!(
                            "[DD_EVAL] Bool/not toggle must target HOLD '{}'; cross-cell toggles are not supported",
                            state_name
                        );
                    }
                }
                StateTransform::Identity
            }
            // Pattern: state + 1 → Increment
            Expression::ArithmeticOperator(ArithmeticOperator::Add { operand_a, .. }) => {
                // Check if left operand is state (Variable declaration or Alias reference)
                let is_state = match &operand_a.node {
                    Expression::Variable(var) => var.name.as_ref() == state_name,
                    Expression::Alias(Alias::WithoutPassed { parts, .. }) => {
                        parts.len() == 1 && parts[0].as_ref() == state_name
                    }
                    _ => false,
                };
                if is_state {
                    return StateTransform::Increment;
                }
                StateTransform::Identity
            }
            // Pattern: True or False tag → SetTrue/SetFalse
            Expression::Literal(Literal::Tag(name)) => {
                match BoolTag::from_tag(name.as_ref()) {
                    Some(BoolTag::True) => StateTransform::SetTrue,
                    Some(BoolTag::False) => StateTransform::SetFalse,
                    None => StateTransform::Identity,
                }
            }
            _ => StateTransform::Identity,
        }
    }

    /// Extract per-input link mappings from a LATEST body inside HOLD.
    /// For patterns like: `LATEST { btn1.press |> THEN { state + 1 }, btn2.press |> THEN { state - 1 } }`
    /// Returns Vec of (link_id, LinkAction) pairs, or None if the pattern isn't recognized.
    fn extract_hold_latest_link_mappings(
        &mut self,
        body: &Expression,
        state_name: &str,
    ) -> Option<Vec<(String, LinkAction)>> {
        let inputs = match body {
            Expression::Latest { inputs, .. } => inputs,
            _ => return None,
        };
        let mut mappings = Vec::new();
        for input in inputs {
            if let Expression::Pipe { from, to } = &input.node {
                if let Expression::Then { body: then_body } = &to.node {
                    let Some(link_id) = self.extract_link_trigger_id(&input.node) else {
                        continue;
                    };
                    match &then_body.node {
                        // state + N
                        Expression::ArithmeticOperator(ArithmeticOperator::Add { operand_a, operand_b }) => {
                            let is_state = match &operand_a.node {
                                Expression::Alias(Alias::WithoutPassed { parts, .. }) =>
                                    parts.len() == 1 && parts[0].as_ref() == state_name,
                                Expression::Variable(var) => var.name.as_ref() == state_name,
                                _ => false,
                            };
                            if is_state {
                                let step = self.eval_expression(&operand_b.node);
                                mappings.push((link_id, LinkAction::AddValue(step)));
                            }
                        }
                        // state - N → AddValue(-N)
                        Expression::ArithmeticOperator(ArithmeticOperator::Subtract { operand_a, operand_b }) => {
                            let is_state = match &operand_a.node {
                                Expression::Alias(Alias::WithoutPassed { parts, .. }) =>
                                    parts.len() == 1 && parts[0].as_ref() == state_name,
                                Expression::Variable(var) => var.name.as_ref() == state_name,
                                _ => false,
                            };
                            if is_state {
                                let step = self.eval_expression(&operand_b.node);
                                if let Value::Number(n) = step {
                                    mappings.push((link_id, LinkAction::AddValue(Value::Number(-n))));
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        if mappings.is_empty() { None } else { Some(mappings) }
    }

    /// Generate a unique HOLD ID using persistence.
    /// When inside a LIST item context (context_path contains "[N]" segments),
    /// appends the index to disambiguate per-item HOLDs from the same source location.
    fn generate_cell_id(&mut self, persistence: Option<&Persistence>) -> String {
        let persistence = persistence.unwrap_or_else(|| {
            panic!("[DD_EVAL] HOLD must have persistence metadata for stable id");
        });
        let base = Self::cell_id_from_persistence(persistence.id);
        // Include full context path for per-call-site uniqueness.
        // This differentiates HOLDs inside functions called multiple times
        // (e.g., store.btn_a.clicked vs store.btn_b.clicked).
        if self.context_path.is_empty() {
            base
        } else {
            format!("{}_{}", base, self.context_path.join("."))
        }
    }

    fn cell_id_from_persistence(persistence_id: PersistenceId) -> String {
        format!("hold_{}", persistence_id)
    }

    /// Snapshot current initial values for any CellRefs found in an item.
    /// Used when building list literals to capture per-item HOLD initial values
    /// before the next item's evaluation overwrites the shared cell configs.
    fn snapshot_cell_values_for_item(&self, item: &Value) -> HashMap<String, Value> {
        let mut snapshot = HashMap::new();
        self.collect_cellref_initial_values(item, &mut snapshot);
        snapshot
    }

    fn collect_cellref_initial_values(&self, value: &Value, snapshot: &mut HashMap<String, Value>) {
        match value {
            Value::CellRef(cell_id) => {
                let name = cell_id.name();
                // Find the LAST CellConfig with this name (most recent evaluation)
                if let Some(cell_config) = self.dataflow_config.cells.iter().rev()
                    .find(|c| c.id.name() == name)
                {
                    snapshot.insert(name, cell_config.initial.clone());
                }
            }
            Value::Object(fields) => {
                for v in fields.values() {
                    self.collect_cellref_initial_values(v, snapshot);
                }
            }
            Value::Tagged { fields, .. } => {
                for v in fields.values() {
                    self.collect_cellref_initial_values(v, snapshot);
                }
            }
            _ => {}
        }
    }

    /// Generate a deterministic LINK ID from the current context path.
    fn generate_link_id(&mut self) -> String {
        if self.context_path.is_empty() {
            panic!("[DD_EVAL] Link must be declared within a named context to derive a stable id");
        }
        self.context_path.join(".")
    }

    /// Generate a deterministic LATEST cell ID from the current context path.
    fn generate_latest_cell_id(&self) -> String {
        if self.context_path.is_empty() {
            panic!("[DD_EVAL] LATEST must be declared within a named context to derive a stable id");
        }
        format!("latest_{}", self.context_path.join("."))
    }

    fn generate_sum_cell_id(&self) -> String {
        if self.context_path.is_empty() {
            panic!("[DD_EVAL] Math/sum must be declared within a named context to derive a stable id");
        }
        format!("sum_{}", self.context_path.join("."))
    }

    /// Push a context segment onto the path.
    fn push_context(&mut self, segment: &str) {
        self.context_path.push(segment.to_string());
    }

    /// Pop the last context segment.
    fn pop_context(&mut self) {
        self.context_path.pop();
    }

    /// Get the current value of a variable.
    pub fn get_variable(&self, name: &str) -> Option<&Value> {
        self.variables.get(name)
    }

    fn with_scoped_context<R>(
        &mut self,
        scoped_vars: HashMap<String, Value>,
        passed_context: Option<Value>,
        f: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let saved_vars = std::mem::replace(&mut self.variables, scoped_vars);
        let saved_passed = self.passed_context.clone();
        self.passed_context = passed_context;

        let result = f(self);

        self.variables = saved_vars;
        self.passed_context = saved_passed;

        result
    }

    /// Get the document output (the root rendering output).
    pub fn get_document(&self) -> Option<&Value> {
        self.get_variable("document")
    }

    /// Inject a variable value before evaluation.
    /// This allows external state (from ReactiveContext) to override
    /// AST-defined variables.
    pub fn inject_variable(&mut self, name: impl Into<String>, value: Value) {
        self.variables.insert(name.into(), value);
    }

    /// Inject multiple variables at once.
    pub fn inject_variables(&mut self, vars: impl IntoIterator<Item = (String, Value)>) {
        for (name, value) in vars {
            self.variables.insert(name, value);
        }
    }

    /// Call a function with arguments and return the result.
    /// Used for evaluating item templates like `new_list_item("placeholder")`.
    pub fn call_function(&mut self, name: &str, args: &[(&str, Value)]) -> Option<Value> {
        let func_def = self.functions.get(name)?.clone();

        // Create a new runtime with the function arguments as variables
        let mut func_runtime = self.fork();

        // Bind arguments to parameters
        for (param, arg_name) in func_def.parameters.iter().zip(args.iter()) {
            func_runtime.variables.insert(param.clone(), arg_name.1.clone());
        }

        // Evaluate the function body
        Some(func_runtime.eval_expression(&func_def.body.node))
    }

    /// Evaluate expressions and store results.
    ///
    /// Does two passes to handle forward references (variables that reference
    /// other variables defined later in the source).
    ///
    /// Pre-injected variables (via `inject_variable`) are preserved and not
    /// overwritten by AST evaluation.
    pub fn evaluate(&mut self, expressions: &[Spanned<Expression>]) {
        // Remember which variables were pre-injected (should not be overwritten)
        let injected_vars: std::collections::HashSet<String> =
            self.variables.keys().cloned().collect();

        // First: collect all function definitions
        for expr in expressions {
            if let Expression::Function { name, parameters, body } = &expr.node {
                let func_name = name.as_str().to_string();
                let params: Vec<String> = parameters.iter().map(|p| p.node.as_str().to_string()).collect();
                self.functions.insert(func_name, FunctionDef {
                    parameters: params,
                    body: body.clone(),
                });
            }
        }

        // Pre-seed all variable names with Unit so forward references resolve
        // instead of panicking during the first pass.
        for expr in expressions {
            if let Expression::Variable(var) = &expr.node {
                let name = var.name.as_str().to_string();
                if !injected_vars.contains(&name) {
                    self.variables.entry(name).or_insert(Value::Unit);
                }
            }
        }

        // First pass: evaluate all variables (forward refs resolve to Unit)
        // Skip pre-injected variables
        for expr in expressions {
            if let Expression::Variable(var) = &expr.node {
                let name = var.name.as_str().to_string();
                if !injected_vars.contains(&name) {
                    self.push_context(&name);
                    let value = self.eval_expression(&var.value.node);
                    self.pop_context();
                    self.variables.insert(name, value);
                }
            }
        }

        // Reset accumulation fields between passes.
        // Pass 1 populated `variables` (needed for forward ref resolution),
        // but dataflow_config, cell_to_collection, and latest_cells contain
        // stale entries with wrong CollectionIds, cell mappings, etc.
        self.dataflow_config = DataflowConfig::new();
        self.cell_to_collection.clear();
        self.latest_cells.clear();

        // Second pass: re-evaluate to resolve forward references
        // Now all variable names are defined, so references should resolve
        // Skip pre-injected variables
        let mut event_sources: HashMap<String, Vec<String>> = HashMap::new();
        for expr in expressions {
            if let Expression::Variable(var) = &expr.node {
                let name = var.name.as_str().to_string();
                if injected_vars.contains(&name) {
                    continue; // Keep pre-injected value
                }
                self.push_context(&name);
                let value = if self.contains_reactive_list_ops(&var.value.node) {
                    // Extract and evaluate the initial list expression (before reactive ops)
                    if let Some(initial_expr) = self.extract_initial_list_expr(&var.value.node) {
                        dd_log!("[DD_EVAL] Pass 2: reactive list '{}' found initial_expr", name);
                        let initial_value = self.eval_expression(initial_expr);
                        dd_log!("[DD_EVAL] Pass 2: reactive list '{}' initial_value = {:?}", name, initial_value);
                        let initial_value = self.bind_list_initial_to_cell(&name, initial_value);
                        let persist = var.value.persistence.is_some();
                        self.dataflow_config.add_cell_initialization(&name, initial_value, persist);
                    } else {
                        dd_log!("[DD_EVAL] Pass 2: reactive list '{}' NO initial_expr found!", name);
                    }

                    // Parse List/remove bindings in top-level expressions
                    self.parse_list_remove_bindings(&name, &var.value.node);

                    // Register List/append/List/clear bindings explicitly
                    if let Some(binding) = self.extract_list_append_binding(&name, &var.value.node, &event_sources) {
                        self.dataflow_config.add_list_append_binding(binding);
                    }

                    Value::CellRef(CellId::new(&name))
                } else {
                    self.eval_expression(&var.value.node)
                };
                #[cfg(debug_assertions)]
                dd_log!("[DD_EVAL] {} = {:?}", name, value);
                self.pop_context();
                self.variables.insert(name.clone(), value);

                let sources = self.collect_event_links(&var.value.node, &event_sources);
                if !sources.is_empty() {
                    event_sources.insert(name, sources);
                }
            }
        }

        // Third pass: handle cascading forward references (depth > 1).
        // Example: document → counter → increment_button requires 3 passes:
        //   Pass 1: increment_button resolved, counter/document still Unit
        //   Pass 2: counter resolved (uses increment_button), document still stale
        //   Pass 3: document resolved (uses updated counter)
        self.dataflow_config = DataflowConfig::new();
        self.cell_to_collection.clear();
        self.latest_cells.clear();

        let mut event_sources: HashMap<String, Vec<String>> = HashMap::new();
        for expr in expressions {
            if let Expression::Variable(var) = &expr.node {
                let name = var.name.as_str().to_string();
                if injected_vars.contains(&name) {
                    continue;
                }
                self.push_context(&name);
                let value = if self.contains_reactive_list_ops(&var.value.node) {
                    if let Some(initial_expr) = self.extract_initial_list_expr(&var.value.node) {
                        dd_log!("[DD_EVAL] Pass 3: reactive list '{}' found initial_expr", name);
                        let initial_value = self.eval_expression(initial_expr);
                        let initial_value = self.bind_list_initial_to_cell(&name, initial_value);
                        let persist = var.value.persistence.is_some();
                        self.dataflow_config.add_cell_initialization(&name, initial_value, persist);
                    }
                    self.parse_list_remove_bindings(&name, &var.value.node);
                    if let Some(binding) = self.extract_list_append_binding(&name, &var.value.node, &event_sources) {
                        self.dataflow_config.add_list_append_binding(binding);
                    }
                    Value::CellRef(CellId::new(&name))
                } else {
                    self.eval_expression(&var.value.node)
                };
                #[cfg(debug_assertions)]
                dd_log!("[DD_EVAL] {} = {:?}", name, value);
                self.pop_context();
                self.variables.insert(name.clone(), value);

                let sources = self.collect_event_links(&var.value.node, &event_sources);
                if !sources.is_empty() {
                    event_sources.insert(name, sources);
                }
            }
        }
    }

    /// Evaluate a single expression.
    fn eval_expression(&mut self, expr: &Expression) -> Value {
        match expr {
            // Literals
            Expression::Literal(lit) => self.eval_literal(lit),

            // Alias: variable reference with optional field path
            Expression::Alias(alias) => self.eval_alias(alias),

            // Object literal: [field: value, ...]
            Expression::Object(obj) => self.eval_object(obj),

            // List literal: LIST { a, b, c }
            Expression::List { items } => {
                let mut values: Vec<Value> = Vec::new();
                let mut per_item_cells: Vec<HashMap<String, Value>> = Vec::new();
                for (idx, spanned) in items.iter().enumerate() {
                    // Push item index context so nested LINK/HOLD get per-item unique IDs.
                    // Without this, LIST { make_counter(), make_counter(), make_counter() }
                    // would share IDs across all items.
                    self.push_context(&format!("[{}]", idx));
                    let item = self.eval_expression(&spanned.node);
                    // Snapshot cell initial values for CellRefs in this item.
                    // This is needed because multiple items from the same function template
                    // share CellRef IDs, and later calls overwrite earlier initial values.
                    let snapshot = self.snapshot_cell_values_for_item(&item);
                    values.push(item);
                    per_item_cells.push(snapshot);
                    self.pop_context();
                }
                let collection_id = self.register_list_literal_collection(values);
                // Store per-item cell values so instantiate_fresh_item can use
                // the correct initial value for each item's HOLDs.
                if per_item_cells.iter().any(|m| !m.is_empty()) {
                    self.dataflow_config.per_item_cell_values.insert(collection_id, per_item_cells);
                }
                self.collection_value(collection_id)
            }

            // Text literal: TEXT { ... }
            Expression::TextLiteral { parts } => self.eval_text_literal(parts),

            // Function call
            Expression::FunctionCall { path, arguments } => {
                self.eval_function_call(path, arguments)
            }

            // Pipe: a |> b
            Expression::Pipe { from, to } => {
                // NOTE: LATEST |> Math/sum() is now handled by Math/sum recognizing LatestRef
                // (see eval_pipe -> FunctionCall -> Math/sum handler)

                // Check for Timer/interval() |> THEN { ... } |> Math/sum() pattern
                // This is the timer-driven accumulator pattern (different from event-driven)
                if self.is_timer_sum_pattern(&from.node, &to.node) {
                    // Extract timer info from the pattern
                    if let Some((_timer_id, interval_ms)) = self.extract_timer_info(&from.node) {
                        let cell_id = "timer_counter";
                        // NOTE: Do NOT call init_cell here!
                        // The test expects empty output until the first timer fires.
                        // The interpreter will handle initialization via DataflowConfig.

                        dd_log!("[DD_EVAL] Timer+sum pattern detected: {} @ {}ms", cell_id, interval_ms);

                        // Task 6.3: Build CellConfig during evaluation (eliminates interpreter fallback)
                        self.add_cell_config(CellConfig {
                            id: CellId::new(cell_id),
                            initial: Value::Unit, // Unit = "not yet rendered" until first timer tick
                            triggered_by: Vec::new(), // Timer-triggered, no external triggers
                            timer_interval_ms: interval_ms,
                            filter: EventFilter::Any,
                            transform: StateTransform::Increment,
                            persist: false, // Timer values are NOT persisted
                        });

                        // Return TimerRef so interpreter can set up the timer
                        return Value::TimerRef {
                            id: Arc::from(cell_id),
                            interval_ms
                        };
                    }
                }

                // NOTE: LATEST |> Router/go_to() is now handled by Router/go_to recognizing LatestRef
                // (see eval_pipe -> FunctionCall -> Router/go_to handler)

                let from_val = self.eval_expression(&from.node);
                self.eval_pipe(&from_val, to)
            }

            // Block: BLOCK { vars, result }
            Expression::Block { variables, output } => {
                let scoped_vars = self.variables.clone();
                let passed_context = self.passed_context.clone();
                self.with_scoped_context(scoped_vars, passed_context, |runtime| {
                    for var in variables {
                        let name = var.node.name.as_str().to_string();
                        let value = runtime.eval_expression(&var.node.value.node);
                        runtime.variables.insert(name, value);
                    }
                    runtime.eval_expression(&output.node)
                })
            }

            // Comparators: ==, !=, <, >, <=, >=
            Expression::Comparator(comp) => self.eval_comparator(comp),

            // Arithmetic operators: +, -, *, /
            Expression::ArithmeticOperator(op) => self.eval_arithmetic(op),

            // LATEST { a, b, c } - merge multiple inputs into a reactive stream
            Expression::Latest { inputs } => {
                self.eval_latest(inputs)
            }

            // HOLD - for static eval, return unit (needs pipe context)
            Expression::Hold { .. } => Value::Unit,

            // THEN - for static eval, return unit (needs event)
            Expression::Then { .. } => Value::Unit,

            // WHEN/WHILE - try to match patterns for static values
            Expression::When { arms } | Expression::While { arms } => {
                // For static eval, need piped value - return Unit (handled in eval_pipe)
                Value::Unit
            }

            // LINK - create a LinkRef with a unique ID for reactive wiring
            Expression::Link => {
                let link_id = self.generate_link_id();
                Value::link_ref(link_id)
            }

            // Skip
            Expression::Skip => Value::Unit,

            // Tagged object
            Expression::TaggedObject { tag, object } => {
                Self::assert_tag_not_reserved(tag.as_str(), "tagged object");
                let fields = self.eval_object(object);
                if let Value::Object(map) = fields {
                    Value::Tagged {
                        tag: Arc::from(tag.as_str()),
                        fields: map,
                    }
                } else {
                    Value::Unit
                }
            }

            // Variable definition (shouldn't appear here normally)
            Expression::Variable(var) => self.eval_expression(&var.value.node),

            // Field access: .field.path
            Expression::FieldAccess { path } => {
                // This should only appear in pipe context
                Value::Unit
            }

            // Fallback for unhandled expressions
            _ => Value::Unit,
        }
    }

    /// Evaluate a literal.
    fn eval_literal(&mut self, lit: &Literal) -> Value {
        match lit {
            Literal::Number(n) => Value::float(*n),
            Literal::Text(s) => Value::text(s.as_str()),
            Literal::Tag(s) => {
                Self::assert_tag_not_reserved(s.as_str(), "tag literal");
                Value::Tagged {
                    tag: Arc::from(s.as_str()),
                    fields: Arc::new(BTreeMap::new()),
                }
            }
        }
    }

    fn assert_tag_not_reserved(tag: &str, context: &str) {
        if tag.starts_with("__") {
            panic!(
                "[DD_EVAL] '{}' is internal; {} must not define reserved tags",
                tag, context
            );
        }
    }

    /// Evaluate an object.
    ///
    /// Uses a scoped runtime so variables defined earlier in the object
    /// can be referenced by variables defined later (forward references
    /// within the same object).
    fn eval_object(&mut self, obj: &Object) -> Value {
        let scoped_vars = self.variables.clone();
        let passed_context = self.passed_context.clone();
        self.with_scoped_context(scoped_vars, passed_context, |runtime| {
            let mut map = BTreeMap::new();
            let mut event_sources: HashMap<String, Vec<String>> = HashMap::new();
            for var in &obj.variables {
                let name_str = var.node.name.as_str();
                if name_str == ITEM_KEY_FIELD {
                    panic!(
                        "[DD_EVAL] '{}' is internal; Boon objects must not define it",
                        ITEM_KEY_FIELD
                    );
                }
                if name_str.starts_with("__") {
                    panic!(
                        "[DD_EVAL] '{}' is internal; Boon objects must not define reserved fields",
                        name_str
                    );
                }
                let name = Arc::from(name_str);
                runtime.push_context(name_str);

                // Check if this field contains reactive list operations (List/append, List/clear)
                // If so, evaluate the initial list, store in HOLD, and return CellRef
                let value = if runtime.contains_reactive_list_ops(&var.node.value.node) {
                    dd_log!("[DD_EVAL] Field '{}' has reactive list ops - creating CellRef", name_str);

                    // Extract and evaluate the initial list expression (before List/append, List/clear, List/remove)
                    if let Some(initial_expr) = runtime.extract_initial_list_expr(&var.node.value.node) {
                        let initial_value = runtime.eval_expression(initial_expr);
                        let initial_value = runtime.bind_list_initial_to_cell(name_str, initial_value);
                        dd_log!("[DD_EVAL] Field '{}' initial list value: {:?}", name_str, initial_value);

                        // Register the initial value for interpreter/worker initialization
                        let persist = var.node.value.persistence.is_some();
                        runtime.dataflow_config.add_cell_initialization(name_str, initial_value, persist);
                    }

                    // Parse List/remove event paths from the pipe chain (since eval_pipe is bypassed)
                    // Store in the main config (no scoped config side effects).
                    runtime.parse_list_remove_bindings(name_str, &var.node.value.node);

                    // Register List/append/List/clear bindings explicitly (no IO registries).
                    if let Some(binding) = runtime.extract_list_append_binding(name_str, &var.node.value.node, &event_sources) {
                        runtime.dataflow_config.add_list_append_binding(binding);
                    }

                    // Build explicit list item template from List/append item expression (no inference).
                    if let Some((template, mappings)) = runtime.build_list_item_template(
                        name_str,
                        &var.node.value.node,
                        &event_sources,
                        runtime.dataflow_config.remove_event_paths.get(name_str).map(|v| v.as_slice()),
                    ) {
                        runtime.dataflow_config.set_list_item_template(name_str, template);
                        for mapping in mappings {
                            runtime.dataflow_config.add_link_mapping(mapping);
                        }
                    }

                    Value::CellRef(CellId::new(name_str))
                } else {
                    runtime.eval_expression(&var.node.value.node)
                };

                // Track event sources for later variables (e.g., List/append item aliases).
                let sources = runtime.collect_event_links(&var.node.value.node, &event_sources);
                if !sources.is_empty() {
                    event_sources.insert(name_str.to_string(), sources);
                }
                runtime.pop_context();

                // Add to both the result map and the scoped runtime
                // so later variables can reference earlier ones
                map.insert(name, value.clone());
                runtime.variables.insert(name_str.to_string(), value);
            }
            Value::Object(Arc::new(map))
        })
    }

    /// Check if an expression contains reactive list operations (List/append, List/clear).
    /// Used to detect fields that should become reactive HOLDs.
    fn contains_reactive_list_ops(&self, expr: &Expression) -> bool {
        match expr {
            Expression::Pipe { from, to } => {
                // Check if piping to List/append, List/clear, or List/remove
                if let Expression::FunctionCall { path, .. } = &to.node {
                    let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                    if path_strs == ["List", "append"]
                        || path_strs == ["List", "clear"]
                        || path_strs == ["List", "remove"]
                    {
                        return true;
                    }
                }
                // Recursively check both sides of pipe
                self.contains_reactive_list_ops(&from.node) || self.contains_reactive_list_ops(&to.node)
            }
            Expression::FunctionCall { path, .. } => {
                let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                path_strs == ["List", "append"]
                    || path_strs == ["List", "clear"]
                    || path_strs == ["List", "remove"]
            }
            _ => false,
        }
    }

    /// Extract the initial list expression from a reactive list pipe chain.
    /// For `LIST { ... } |> List/append(...) |> List/remove(...)`, returns the `LIST { ... }` part.
    fn extract_initial_list_expr<'a>(&self, expr: &'a Expression) -> Option<&'a Expression> {
        match expr {
            Expression::Pipe { from, to } => {
                // Check if the `to` part is a reactive list operation
                if let Expression::FunctionCall { path, .. } = &to.node {
                    let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                    if path_strs == ["List", "append"] || path_strs == ["List", "clear"] || path_strs == ["List", "remove"] {
                        // Recursively check if `from` also has reactive ops, or return it
                        if self.contains_reactive_list_ops(&from.node) {
                            return self.extract_initial_list_expr(&from.node);
                        } else {
                            // This is the initial list expression
                            return Some(&from.node);
                        }
                    }
                }
                // Not a reactive list op at this level, check deeper
                self.extract_initial_list_expr(&from.node)
                    .or_else(|| self.extract_initial_list_expr(&to.node))
            }
            _ => None,
        }
    }

    /// Parse List/remove bindings from a pipe chain expression.
    /// This is called when reactive list ops are detected but eval_pipe is bypassed.
    /// Walks the expression tree to find all List/remove calls and extracts their `on:` event paths.
    fn parse_list_remove_bindings(&mut self, list_cell_id: &str, expr: &Expression) {
        match expr {
            Expression::Pipe { from, to } => {
                // First check the `to` part for List/remove
                if let Expression::FunctionCall { path, arguments } = &to.node {
                    let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                    if path_strs == ["List", "remove"] {
                        // Get the binding name (first argument without value, e.g., "item")
                        let binding_name = arguments
                            .iter()
                            .find(|arg| arg.node.value.is_none())
                            .map(|arg| arg.node.name.as_str());

                        // Get the "on:" event expression (unevaluated)
                        let on_expr = arguments
                            .iter()
                            .find(|arg| arg.node.name.as_str() == "on")
                            .and_then(|arg| arg.node.value.as_ref());

                        if let (Some(binding), Some(event_expr)) = (binding_name, on_expr) {
                            self.register_list_remove_binding(list_cell_id, binding, &event_expr.node);
                        }
                    }
                }
                // Recursively check both sides of pipe
                self.parse_list_remove_bindings(list_cell_id, &from.node);
                self.parse_list_remove_bindings(list_cell_id, &to.node);
            }
            Expression::FunctionCall { path, arguments } => {
                // Direct function call (not in pipe context)
                let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                if path_strs == ["List", "remove"] {
                    let binding_name = arguments
                        .iter()
                        .find(|arg| arg.node.value.is_none())
                        .map(|arg| arg.node.name.as_str());
                    let on_expr = arguments
                        .iter()
                        .find(|arg| arg.node.name.as_str() == "on")
                        .and_then(|arg| arg.node.value.as_ref());

                    if let (Some(binding), Some(event_expr)) = (binding_name, on_expr) {
                        self.register_list_remove_binding(list_cell_id, binding, &event_expr.node);
                    }
                }
            }
            _ => {}
        }
    }

    /// Extract List/append and List/clear bindings from a pipe chain expression.
    ///
    /// Returns None if no List/append is present.
    fn extract_list_append_binding(
        &self,
        list_cell_id: &str,
        expr: &Expression,
        local_sources: &HashMap<String, Vec<String>>,
    ) -> Option<ListAppendBinding> {
        let mut append_links: Vec<String> = Vec::new();
        let mut clear_links: Vec<String> = Vec::new();

        fn visit(
            runtime: &BoonDdRuntime,
            expr: &Expression,
            local_sources: &HashMap<String, Vec<String>>,
            list_cell_id: &str,
            append_links: &mut Vec<String>,
            clear_links: &mut Vec<String>,
        ) {
            match expr {
                Expression::Pipe { from, to } => {
                    visit(runtime, &from.node, local_sources, list_cell_id, append_links, clear_links);
                    visit(runtime, &to.node, local_sources, list_cell_id, append_links, clear_links);
                }
                Expression::FunctionCall { path, arguments } => {
                    let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                    if path_strs == ["List", "append"] {
                        let item_expr = arguments
                            .iter()
                            .find(|arg| arg.node.name.as_str() == "item")
                            .and_then(|arg| arg.node.value.as_ref())
                            .map(|v| &v.node)
                            .unwrap_or_else(|| {
                                panic!("[DD_EVAL] Bug: List/append requires item: argument");
                            });
                        let links = runtime.collect_event_links(item_expr, local_sources);
                        if links.is_empty() {
                            panic!("[DD_EVAL] Bug: List/append item has no event source for list '{}'", list_cell_id);
                        }
                        append_links.extend(links);
                    }
                    if path_strs == ["List", "clear"] {
                        let on_expr = arguments
                            .iter()
                            .find(|arg| arg.node.name.as_str() == "on")
                            .and_then(|arg| arg.node.value.as_ref())
                            .map(|v| &v.node)
                            .unwrap_or_else(|| {
                                panic!("[DD_EVAL] Bug: List/clear requires on: argument");
                            });
                        let (path, _) = runtime
                            .extract_event_path(on_expr)
                            .unwrap_or_else(|| {
                                panic!("[DD_EVAL] Bug: List/clear on: must be an event path");
                            });
                        let link_val = runtime
                            .resolve_field_path(&path)
                            .unwrap_or_else(|| {
                                panic!("[DD_EVAL] Bug: List/clear on: could not resolve path {:?}", path);
                            });
                        let Value::LinkRef(link_id) = link_val else {
                            panic!("[DD_EVAL] Bug: List/clear on: expected LinkRef at {:?}", path);
                        };
                        clear_links.push(link_id.to_string());
                    }

                    for arg in arguments {
                        if let Some(value) = &arg.node.value {
                            visit(runtime, &value.node, local_sources, list_cell_id, append_links, clear_links);
                        }
                    }
                }
                Expression::Latest { inputs } => {
                    for input in inputs {
                        visit(runtime, &input.node, local_sources, list_cell_id, append_links, clear_links);
                    }
                }
                Expression::When { arms } | Expression::While { arms } => {
                    for arm in arms {
                        visit(runtime, &arm.body.node, local_sources, list_cell_id, append_links, clear_links);
                    }
                }
                Expression::Block { variables, output } => {
                    for var in variables {
                        visit(runtime, &var.node.value.node, local_sources, list_cell_id, append_links, clear_links);
                    }
                    visit(runtime, &output.node, local_sources, list_cell_id, append_links, clear_links);
                }
                Expression::Object(obj) => {
                    for var in &obj.variables {
                        visit(runtime, &var.node.value.node, local_sources, list_cell_id, append_links, clear_links);
                    }
                }
                Expression::List { items } => {
                    for item in items {
                        visit(runtime, &item.node, local_sources, list_cell_id, append_links, clear_links);
                    }
                }
                _ => {}
            }
        }

        visit(self, expr, local_sources, list_cell_id, &mut append_links, &mut clear_links);

        if append_links.is_empty() {
            return None;
        }
        append_links.sort();
        append_links.dedup();
        clear_links.sort();
        clear_links.dedup();

        Some(ListAppendBinding {
            list_cell_id: list_cell_id.to_string(),
            append_link_ids: append_links,
            clear_link_ids: clear_links,
        })
    }

    /// Extract the List/append item expression from a pipe chain.
    /// Returns None if no List/append is present.
    fn extract_list_append_item_expr<'a>(&self, expr: &'a Expression) -> Option<&'a Expression> {
        let mut found: Option<&'a Expression> = None;

        fn visit<'a>(expr: &'a Expression, found: &mut Option<&'a Expression>) {
            match expr {
                Expression::Pipe { from, to } => {
                    visit(&from.node, found);
                    visit(&to.node, found);
                }
                Expression::FunctionCall { path, arguments } => {
                    let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                    if path_strs == ["List", "append"] {
                        let item_expr = arguments
                            .iter()
                            .find(|arg| arg.node.name.as_str() == "item")
                            .and_then(|arg| arg.node.value.as_ref())
                            .map(|v| &v.node)
                            .unwrap_or_else(|| {
                                panic!("[DD_EVAL] Bug: List/append requires item: argument");
                            });
                        if found.is_some() {
                            panic!("[DD_EVAL] Bug: Multiple List/append item expressions found");
                        }
                        *found = Some(item_expr);
                    }
                    for arg in arguments {
                        if let Some(value) = &arg.node.value {
                            visit(&value.node, found);
                        }
                    }
                }
                Expression::Latest { inputs } => {
                    for input in inputs {
                        visit(&input.node, found);
                    }
                }
                Expression::When { arms } | Expression::While { arms } => {
                    for arm in arms {
                        visit(&arm.body.node, found);
                    }
                }
                Expression::Block { variables, output } => {
                    for var in variables {
                        visit(&var.node.value.node, found);
                    }
                    visit(&output.node, found);
                }
                Expression::Object(obj) => {
                    for var in &obj.variables {
                        visit(&var.node.value.node, found);
                    }
                }
                Expression::List { items } => {
                    for item in items {
                        visit(&item.node, found);
                    }
                }
                _ => {}
            }
        }

        visit(expr, &mut found);
        found
    }

    /// Build a template evaluation runtime with event-derived variables replaced by Placeholder.
    fn build_template_runtime(&self, event_sources: &HashMap<String, Vec<String>>) -> BoonDdRuntime {
        let mut template_runtime = self.fork();

        for name in event_sources.keys() {
            template_runtime.variables.insert(name.clone(), Value::Placeholder);
        }

        template_runtime
    }

    fn value_contains_cell_ref(value: &Value) -> bool {
        match value {
            Value::CellRef(_) => true,
            Value::PlaceholderField(_) => true,
            Value::WhileConfig(config) => {
                config.arms.iter().any(|arm| {
                    Self::value_contains_cell_ref(&arm.pattern) || Self::value_contains_cell_ref(&arm.body)
                }) || Self::value_contains_cell_ref(&config.default)
            }
            Value::PlaceholderWhile(config) => {
                config.arms.iter().any(|arm| {
                    Self::value_contains_cell_ref(&arm.pattern) || Self::value_contains_cell_ref(&arm.body)
                }) || Self::value_contains_cell_ref(&config.default)
            }
            Value::Object(fields) => fields.values().any(Self::value_contains_cell_ref),
            Value::Tagged { fields, .. } => fields.values().any(Self::value_contains_cell_ref),
            _ => false,
        }
    }

    fn collect_cell_initials(config: &DataflowConfig) -> HashMap<String, Value> {
        let mut initials = HashMap::new();
        for cell in &config.cells {
            initials.insert(cell.id.name(), cell.initial.clone());
        }
        for (cell_id, init) in &config.cell_initializations {
            initials.insert(cell_id.clone(), init.value.clone());
        }
        initials
    }

    /// Bind a list initial value to a specific hold cell at compile time.
    /// This is the single place where a list handle without cell binding is accepted.
    fn bind_list_initial_to_cell(&mut self, cell_id: &str, value: Value) -> Value {
        match value {
            Value::List(handle) => {
                let collection_id = self.get_collection_id(cell_id).unwrap_or_else(|| {
                    self.dataflow_config
                        .add_collection_source(handle.id, cell_id.to_string());
                    self.cell_to_collection
                        .insert(cell_id.to_string(), handle.id);
                    handle.id
                });
                if handle.id != collection_id {
                    // Second pass produced a new CollectionId — update mapping.
                    // This is expected: pass 1 creates draft collections, pass 2
                    // re-evaluates with all forward refs resolved, yielding new IDs.
                    dd_log!(
                        "[DD_EVAL] Updating collection mapping for '{}': {:?} -> {:?}",
                        cell_id, collection_id, handle.id
                    );
                    self.dataflow_config
                        .add_collection_source(handle.id, cell_id.to_string());
                    self.cell_to_collection
                        .insert(cell_id.to_string(), handle.id);
                }
                if !self.dataflow_config.initial_collections.contains_key(&collection_id) {
                    panic!(
                        "[DD_EVAL] Missing initial items for list hold '{}'",
                        cell_id
                    );
                }
                if let Some(existing) = handle.cell_id.as_deref() {
                    if existing != cell_id {
                        panic!(
                            "[DD_EVAL] Collection cell_id mismatch for '{}': found '{}'",
                            cell_id, existing
                        );
                    }
                    Value::List(handle)
                } else {
                    Value::List(CollectionHandle::with_id_and_cell(handle.id, Arc::from(cell_id)))
                }
            }
            other => {
                panic!(
                    "[DD_EVAL] Initial list value for '{}' must be Collection, found {:?}",
                    cell_id, other
                );
            }
        }
    }

    /// Build an explicit ListItemTemplate and template link mappings from List/append item expression.
    fn build_list_item_template(
        &self,
        list_cell_id: &str,
        expr: &Expression,
        event_sources: &HashMap<String, Vec<String>>,
        remove_event_path: Option<&[String]>,
    ) -> Option<(ListItemTemplate, Vec<LinkCellMapping>)> {
        let item_expr = self.extract_list_append_item_expr(expr)?;
        // If there's no List/remove path, items are simple values without per-item
        // identity tracking (e.g., append + clear only). Skip template building.
        let remove_event_path = remove_event_path?;

        let mut template_runtime = self.build_template_runtime(event_sources);
        let data_template = template_runtime.eval_expression(item_expr);
        if !Self::value_contains_cell_ref(&data_template) {
            return None;
        }

        let Value::Object(fields) = &data_template else {
            panic!("[DD_EVAL] Bug: List/append template must be an Object");
        };

        if remove_event_path.is_empty() {
            panic!("[DD_EVAL] Bug: List/append template requires non-empty identity path");
        }

        let template_config = template_runtime.take_config();
        let cell_initials = Self::collect_cell_initials(&template_config);

        let mut field_initializers: Vec<(Vec<String>, FieldInitializer)> = Vec::new();
        let mut persisted_fields: Vec<(String, Vec<String>)> = Vec::new();

        for (field_name, value) in fields.iter() {
            if let Value::CellRef(cell_id) = value {
                let initial = cell_initials.get(&cell_id.name()).unwrap_or_else(|| {
                    panic!(
                        "[DD_EVAL] Bug: missing initial value for template hold {}",
                        cell_id.name()
                    );
                });
                let initializer = match initial {
                    Value::Placeholder => FieldInitializer::FromEventText,
                    Value::PlaceholderField(_) => {
                        FieldInitializer::FromEventText
                    }
                    other => FieldInitializer::Constant(other.clone()),
                };
                field_initializers.push((vec![field_name.to_string()], initializer));
                persisted_fields.push((field_name.to_string(), vec![field_name.to_string()]));
            }
        }

        if field_initializers.is_empty() {
            return None;
        }

        let template = ListItemTemplate {
            data_template: TemplateValue::from_value(data_template.clone()),
            element_template: None,
            identity: ItemIdentitySpec {
                link_ref_path: remove_event_path.to_vec(),
            },
            field_initializers,
            link_actions: Vec::new(),
            persisted_fields,
        };

        let mut mappings: Vec<LinkCellMapping> = Vec::new();

        // RemoveListItem mapping for template
        let remove_link_id = get_link_ref_at_path(&data_template, remove_event_path)
            .unwrap_or_else(|| {
                panic!(
                    "[DD_EVAL] Bug: remove_event_path {:?} did not resolve in template",
                    remove_event_path
                );
            });
        mappings.push(LinkCellMapping::remove_list_item(
            remove_link_id,
            list_cell_id.to_string(),
        ));

        // Link mappings captured during template evaluation (bool toggles, set true/false, etc.)
        mappings.extend(template_config.link_mappings.clone());

        Some((template, mappings))
    }

    /// Collect event source LinkRef IDs from an expression.
    fn collect_event_links(
        &self,
        expr: &Expression,
        local_sources: &HashMap<String, Vec<String>>,
    ) -> Vec<String> {
        let mut links: Vec<String> = Vec::new();

        fn visit(
            runtime: &BoonDdRuntime,
            expr: &Expression,
            local_sources: &HashMap<String, Vec<String>>,
            links: &mut Vec<String>,
        ) {
            match expr {
                Expression::Alias(Alias::WithoutPassed { parts, .. }) => {
                    if parts.len() == 1 {
                        if let Some(stored) = local_sources.get(parts[0].as_ref()) {
                            links.extend(stored.clone());
                        }
                    }
                    if let Some((path, _)) = runtime.extract_event_path(expr) {
                        // Template binding variables (e.g., `item` in List/remove) can't be
                        // resolved during evaluation — they only exist at runtime. Skip them.
                        if let Some(link_val) = runtime.resolve_field_path(&path) {
                            if let Value::LinkRef(link_id) = link_val {
                                links.push(link_id.to_string());
                            }
                        }
                    }
                }
                Expression::FieldAccess { .. } => {
                    if let Some((path, _)) = runtime.extract_event_path(expr) {
                        if let Some(link_val) = runtime.resolve_field_path(&path) {
                            if let Value::LinkRef(link_id) = link_val {
                                links.push(link_id.to_string());
                            }
                        }
                    }
                }
                Expression::Pipe { from, to } => {
                    visit(runtime, &from.node, local_sources, links);
                    visit(runtime, &to.node, local_sources, links);
                }
                Expression::Latest { inputs } => {
                    for input in inputs {
                        visit(runtime, &input.node, local_sources, links);
                    }
                }
                Expression::When { arms } | Expression::While { arms } => {
                    for arm in arms {
                        visit(runtime, &arm.body.node, local_sources, links);
                    }
                }
                Expression::Block { variables, output } => {
                    for var in variables {
                        visit(runtime, &var.node.value.node, local_sources, links);
                    }
                    visit(runtime, &output.node, local_sources, links);
                }
                Expression::FunctionCall { arguments, .. } => {
                    for arg in arguments {
                        if let Some(value) = &arg.node.value {
                            visit(runtime, &value.node, local_sources, links);
                        }
                    }
                }
                Expression::Object(obj) => {
                    for var in &obj.variables {
                        visit(runtime, &var.node.value.node, local_sources, links);
                    }
                }
                Expression::List { items } => {
                    for item in items {
                        visit(runtime, &item.node, local_sources, links);
                    }
                }
                _ => {}
            }
        }

        visit(self, expr, local_sources, &mut links);
        links.sort();
        links.dedup();
        links
    }

    /// Evaluate a text literal with interpolation.
    /// If any interpolated value is reactive (CellRef, WhileConfig),
    /// create a ComputedText DD cell for reactive text rendering.
    fn eval_text_literal(&mut self, parts: &[TextPart]) -> Value {
        use super::super::core::collection_ops::ComputedTextPart;

        // First pass: collect values
        let mut collected_parts: Vec<Value> = Vec::new();

        for part in parts {
            match part {
                TextPart::Text(s) => {
                    collected_parts.push(Value::text(s.as_str()));
                }
                TextPart::Interpolation { var, .. } => {
                    if var.as_str().contains("|>") {
                        collected_parts.push(Value::text(""));
                        continue;
                    }
                    let value = self.variables.get(var.as_str()).cloned().unwrap_or_else(|| {
                        let parts: Vec<String> = var.as_str().split('.').map(String::from).collect();
                        self.resolve_field_path(&parts).unwrap_or_else(|| {
                            panic!("[DD_EVAL] Bug: missing interpolated variable '{}'", var)
                        })
                    });
                    collected_parts.push(value);
                }
            }
        }

        // Check if any part contains Placeholder markers (inside Map template context).
        // If so, produce a __text_template__ Tagged value that preserves the parts
        // for later resolution during substitute_placeholders.
        let has_placeholder = collected_parts.iter().any(|v| {
            matches!(v, Value::Placeholder | Value::PlaceholderField(_) | Value::PlaceholderBoolNot(_))
        });
        if has_placeholder {
            let mut fields = std::collections::BTreeMap::new();
            for (i, part) in collected_parts.into_iter().enumerate() {
                fields.insert(Arc::from(i.to_string().as_str()), part);
            }
            return Value::Tagged {
                tag: Arc::from("__text_template__"),
                fields: Arc::new(fields),
            };
        }

        // Check if any part is reactive (CellRef or WhileConfig)
        let has_reactive_cellref = collected_parts.iter().any(|v| {
            matches!(v, Value::CellRef(_) | Value::WhileConfig(_))
        });

        if has_reactive_cellref {
            // Check if all CellRefs have collection op backing.
            // HOLD cells (boolean toggle, increment, etc.) don't have collection ops —
            // for these, fall back to __text_template__ which the bridge renders via cell_signal.
            let all_cellrefs_have_collections = collected_parts.iter().all(|v| match v {
                Value::CellRef(cell_id) => self.try_get_collection_id_for_cellref(cell_id).is_some(),
                _ => true,
            });
            if !all_cellrefs_have_collections {
                let mut fields = std::collections::BTreeMap::new();
                for (i, part) in collected_parts.into_iter().enumerate() {
                    fields.insert(Arc::from(i.to_string().as_str()), part);
                }
                return Value::Tagged {
                    tag: Arc::from("__text_template__"),
                    fields: Arc::new(fields),
                };
            }

            // Reactive text interpolation: create ComputedText DD operator.
            // Convert each reactive part to a CellRef with a CollectionId,
            // then build a ComputedText that watches all sources.
            let mut sources: Vec<super::super::core::value::CollectionId> = Vec::new();
            let mut template_parts: Vec<ComputedTextPart> = Vec::new();
            // Map from CollectionId name to source index (dedup shared sources)
            let mut source_index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

            for value in &collected_parts {
                match value {
                    Value::CellRef(cell_id) => {
                        let col_name = cell_id.name();
                        let idx = if let Some(&existing_idx) = source_index.get(&col_name) {
                            existing_idx
                        } else if let Some(col_id) = self.try_get_collection_id_for_cellref(cell_id) {
                            let idx = sources.len();
                            sources.push(col_id);
                            source_index.insert(col_name, idx);
                            idx
                        } else {
                            // CellRef not backed by collection op (e.g., HOLD cell)
                            // Fall back to empty string for now
                            dd_log!("[DD_EVAL] TEXT interpolation: CellRef '{}' not in cell_to_collection, using empty", cell_id.name());
                            template_parts.push(ComputedTextPart::Static(Arc::from("")));
                            continue;
                        };
                        template_parts.push(ComputedTextPart::CellSource(idx));
                    }
                    Value::WhileConfig(config) => {
                        // Convert WhileConfig to ScalarWhen CellRef for text interpolation
                        let cell_name = config.cell_id.name();
                        if let Some(source_col) = self.get_collection_id(&cell_name) {
                            let arms: Vec<(Value, Value)> = config.arms.iter()
                                .map(|arm| (arm.pattern.clone(), arm.body.clone()))
                                .collect();
                            let default = (*config.default).clone();
                            let scalar_ref = self.create_scalar_when(source_col, arms, default);
                            if let Value::CellRef(scalar_cell_id) = &scalar_ref {
                                let col_id = self.try_get_collection_id_for_cellref(scalar_cell_id).unwrap();
                                let idx = sources.len();
                                sources.push(col_id);
                                source_index.insert(scalar_cell_id.name(), idx);
                                template_parts.push(ComputedTextPart::CellSource(idx));
                            }
                        } else {
                            dd_log!("[DD_EVAL] TEXT interpolation: WhileConfig cell '{}' not in cell_to_collection", cell_name);
                            template_parts.push(ComputedTextPart::Static(Arc::from("[while]")));
                        }
                    }
                    Value::Text(s) => {
                        template_parts.push(ComputedTextPart::Static(s.clone()));
                    }
                    _ => {
                        // Static display for other types (Number, Bool, etc.)
                        template_parts.push(ComputedTextPart::Static(Arc::from(value.to_display_string())));
                    }
                }
            }

            if sources.is_empty() {
                // All reactive parts failed to resolve — fall back to static text
                let result: String = template_parts.iter()
                    .map(|p| match p {
                        ComputedTextPart::Static(s) => s.to_string(),
                        ComputedTextPart::CellSource(_) => String::new(),
                    })
                    .collect();
                return Value::text(result);
            }

            dd_log!("[DD_EVAL] TEXT interpolation: creating ComputedText with {} sources, {} parts",
                sources.len(), template_parts.len());
            return self.create_computed_text(sources, template_parts);
        }

        // Static text: no reactive parts, build string directly
        let result: String = collected_parts.iter()
            .map(|v| match v {
                Value::Placeholder | Value::PlaceholderField(_) | Value::PlaceholderBoolNot(_) => {
                    String::new()
                }
                _ => v.to_display_string(),
            })
            .collect();
        Value::text(result)
    }

    /// Evaluate a function call.
    fn eval_function_call(
        &mut self,
        path: &[crate::parser::StrSlice],
        arguments: &[Spanned<crate::parser::static_expression::Argument>],
    ) -> Value {
        // Convert path to namespace/name first (needed to detect Element functions)
        let full_path: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
        let (namespace, name) = if full_path.len() >= 2 {
            (Some(full_path[0]), full_path[1])
        } else if full_path.len() == 1 {
            (None, full_path[0])
        } else {
            panic!("[DD_EVAL] Bug: empty function path");
        };

        // For Element functions, use scoped evaluation where `element` argument
        // is made available as a variable when evaluating other arguments like `items`.
        // This enables patterns like: Element/stripe(element: [hovered: LINK], items: LIST { element.hovered |> WHILE {...} })
        if namespace == Some("Element") {
            return self.eval_element_function_with_scoped_args(name, arguments);
        }

        // Build argument map (standard evaluation for non-Element functions)
        let args: HashMap<&str, Value> = arguments
            .iter()
            .filter_map(|arg| {
                let name = arg.node.name.as_str();
                let value = arg.node.value.as_ref().map(|v| self.eval_expression(&v.node))?;
                Some((name, value))
            })
            .collect();

        match (namespace, name) {
            // Document/new(root: value)
            (Some("Document"), "new") => {
                args.get("root").cloned().unwrap_or_else(|| {
                    panic!("[DD_EVAL] Document/new requires root argument");
                })
            }

            // Math/sum() - returns 0 for static
            (Some("Math"), "sum") => Value::int(0),

            // Timer/interval - returns unit in static context
            (Some("Timer"), "interval") => Value::Unit,

            // Stream/pulses - returns unit in static context
            (Some("Stream"), "pulses") => Value::Unit,

            // Element functions (handled above with scoped args)
            (Some("Element"), _) => unreachable!(),

            // List functions
            (Some("List"), func) => self.eval_list_function(func, &args),

            // Router functions
            (Some("Router"), func) => self.eval_router_function(func, &args),

            // Text functions
            (Some("Text"), func) => self.eval_text_function(func, &args),

            // User-defined function (no namespace, single name)
            (None, func_name) => {
                self.eval_user_function(func_name, &args)
            }

            // Unknown
            _ => panic!("[DD_EVAL] Unknown function call: {:?}::{:?}", namespace, name),
        }
    }

    /// Evaluate Element function with scoped argument evaluation.
    /// The `element` argument is evaluated first and bound as a variable
    /// so other arguments (like `items`) can reference `element.hovered` etc.
    fn eval_element_function_with_scoped_args(
        &mut self,
        func_name: &str,
        arguments: &[Spanned<crate::parser::static_expression::Argument>],
    ) -> Value {
        // Find and evaluate `element` argument first (if present)
        let element_arg = arguments.iter().find(|arg| arg.node.name.as_str() == "element");
        let element_value = element_arg
            .and_then(|arg| arg.node.value.as_ref())
            .map(|v| self.eval_expression(&v.node))
            .unwrap_or_else(|| {
                panic!("[DD_EVAL] Element/{} requires element argument", func_name);
            });

        // Bind `element` as a variable in scope for evaluating remaining arguments
        let old_element = self.variables.insert("element".to_string(), element_value.clone());

        // Build argument map with scoped evaluation
        let args: HashMap<&str, Value> = arguments
            .iter()
            .filter_map(|arg| {
                let name = arg.node.name.as_str();
                if name == "element" {
                    // Already evaluated, use cached value
                    Some((name, element_value.clone()))
                } else {
                    let value = arg.node.value.as_ref().map(|v| self.eval_expression(&v.node))?;
                    Some((name, value))
                }
            })
            .collect();

        // Restore previous `element` variable (if any)
        if let Some(old) = old_element {
            self.variables.insert("element".to_string(), old);
        } else {
            self.variables.remove("element");
        }

        self.eval_element_function(func_name, &args)
    }

    /// Evaluate a user-defined function call.
    fn eval_user_function(&mut self, name: &str, args: &HashMap<&str, Value>) -> Value {
        if let Some(func_def) = self.functions.get(name).cloned() {
            // Check for PASS: argument to set passed_context
            let passed_context = args.get("PASS").cloned().or_else(|| self.passed_context.clone());

            let mut scoped_vars = self.variables.clone();
            // Bind arguments to parameters
            for (param, arg_name) in func_def.parameters.iter().zip(args.keys()) {
                if let Some(value) = args.get(*arg_name) {
                    scoped_vars.insert(param.clone(), value.clone());
                }
            }

            // Also bind by parameter name directly (for named arguments)
            for param in &func_def.parameters {
                if let Some(value) = args.get(param.as_str()) {
                    scoped_vars.insert(param.clone(), value.clone());
                }
            }

            self.with_scoped_context(scoped_vars, passed_context, |runtime| {
                runtime.eval_expression(&func_def.body.node)
            })
        } else {
            panic!("[DD_EVAL] Unknown function '{}'", name);
        }
    }

    /// Evaluate a user-defined function with a piped value.
    /// The piped value becomes the first parameter of the function.
    fn eval_user_function_with_piped(
        &mut self,
        name: &str,
        piped: &Value,
        args: &HashMap<&str, Value>,
    ) -> Value {
        if let Some(func_def) = self.functions.get(name).cloned() {
            // Check for PASS: argument to set passed_context
            let passed_context = args.get("PASS").cloned().or_else(|| self.passed_context.clone());

            let mut scoped_vars = self.variables.clone();
            // First parameter gets the piped value
            if let Some(first_param) = func_def.parameters.first() {
                scoped_vars.insert(first_param.clone(), piped.clone());
            }

            // Bind remaining named arguments
            for param in &func_def.parameters {
                if let Some(value) = args.get(param.as_str()) {
                    scoped_vars.insert(param.clone(), value.clone());
                }
            }

            return self.with_scoped_context(scoped_vars, passed_context, |runtime| {
                runtime.eval_expression(&func_def.body.node)
            });
        } else {
            panic!("[DD_EVAL] Unknown function '{}'", name);
        }
    }

    /// Evaluate an Element function.
    fn eval_element_function(&mut self, name: &str, args: &HashMap<&str, Value>) -> Value {
        dd_log!("[DD_EVAL] Element/{}() called with args: {:?}", name, args.keys().collect::<Vec<_>>());
        if name == "text_input" {
            let text_cell_id = args.get("text").and_then(|v| {
                if let Value::CellRef(cell_id) = v {
                    Some(cell_id.clone())
                } else {
                    None
                }
            });

            let change_link_value = args.get("element")
                .and_then(|e| e.get("event"))
                .and_then(|e| e.get("change"));
            let change_link_id = match change_link_value {
                Some(Value::LinkRef(link_id)) => Some(link_id.clone()),
                Some(other) => {
                    panic!("[DD_EVAL] Element/text_input element.event.change must be LinkRef, found {:?}", other);
                }
                None => None,
            };

            if let Some(cell_id) = text_cell_id {
                let change_link_id = change_link_id.unwrap_or_else(|| {
                    panic!("[DD_EVAL] Element/text_input with CellRef text requires element.event.change LinkRef");
                });
                self.dataflow_config.add_link_mapping(LinkCellMapping::new(
                    change_link_id,
                    cell_id,
                    LinkAction::SetText,
                ));
            }
        }
        let mut fields: Vec<(&str, Value)> = vec![("_element_type", Value::text(name))];
        for (k, v) in args {
            fields.push((k, v.clone()));
        }

        // NOTE: List/append/List/clear bindings are parsed from Boon code, not element scanning.

        let result = Value::tagged("Element", fields.into_iter());
        dd_log!("[DD_EVAL] Element/{}() -> Tagged(Element)", name);
        result
    }

    /// Evaluate a List function.
    fn eval_list_function(&mut self, name: &str, args: &HashMap<&str, Value>) -> Value {
        match name {
            "count" => {
                let value = args.values().next().unwrap_or_else(|| {
                    panic!("[DD_EVAL] List/count expects argument");
                });
                match value {
                    Value::CellRef(cell_id) => self.create_list_count(&cell_id.name()),
                    Value::List(handle) => self.chain_count_on_collection(handle.id),
                    other => {
                        panic!("[DD_EVAL] List/count expects list-like argument, found {:?}", other);
                    }
                }
            }
            "is_empty" => {
                let value = args.values().next().unwrap_or_else(|| {
                    panic!("[DD_EVAL] List/is_empty expects argument");
                });
                match value {
                    Value::CellRef(cell_id) => self.create_list_is_empty(&cell_id.name()),
                    Value::List(handle) => self.chain_is_empty_on_collection(handle.id),
                    other => {
                        panic!("[DD_EVAL] List/is_empty expects list-like argument, found {:?}", other);
                    }
                }
            }
            _ => panic!("[DD_EVAL] Unknown List function '{}'", name),
        }
    }

    /// Evaluate a Router function.
    fn eval_router_function(&mut self, name: &str, _args: &HashMap<&str, Value>) -> Value {
        match name {
            // Router/route() - returns a CellRef to the current route
            // The actual route value is stored in CELL_STATES["current_route"]
            // and updated by navigation events via DD route-change events.
            "route" => {
                // Register route cell for interpreter initialization (no IO in evaluator).
                self.dataflow_config.add_route_cell("current_route");
                // Return CellRef so WHEN can observe route changes reactively
                Value::CellRef(CellId::new("current_route"))
            }
            // Router/go_to(route) - navigation (no-op in static context)
            "go_to" => Value::Unit,
            _ => panic!("[DD_EVAL] Unknown Router function '{}'", name),
        }
    }

    fn require_bool(&self, value: &Value, context: &str) -> bool {
        match value {
            Value::Bool(b) => *b,
            Value::Tagged { tag, .. } if BoolTag::is_bool_tag(tag.as_ref()) => {
                BoolTag::is_true(tag.as_ref())
            }
            other => {
                panic!("[DD_EVAL] {} expects Bool/BoolTag, found {:?}", context, other);
            }
        }
    }

    /// Try to extract a concrete boolean from a Value, returning None for reactive values
    /// (LinkRef, CellRef, etc.) that can't be resolved statically.
    fn try_as_bool(&self, value: &Value) -> Option<bool> {
        match value {
            Value::Bool(b) => Some(*b),
            Value::Tagged { tag, .. } if BoolTag::is_bool_tag(tag.as_ref()) => {
                Some(BoolTag::is_true(tag.as_ref()))
            }
            _ => None,
        }
    }

    /// Evaluate a Text function.
    fn eval_text_function(&mut self, name: &str, args: &HashMap<&str, Value>) -> Value {
        match name {
            // Text/trim() - trim whitespace
            "trim" => {
                if let Some(Value::Text(s)) = args.values().next() {
                    Value::text(s.trim())
                } else {
                    panic!("[DD_EVAL] Text/trim expects Text argument");
                }
            }
            // Text/is_not_empty() -> Bool
            "is_not_empty" => {
                if let Some(Value::Text(s)) = args.values().next() {
                    Value::Bool(!s.is_empty())
                } else {
                    panic!("[DD_EVAL] Text/is_not_empty expects Text argument");
                }
            }
            // Text/is_empty() -> Bool
            "is_empty" => {
                if let Some(Value::Text(s)) = args.values().next() {
                    Value::Bool(s.is_empty())
                } else {
                    panic!("[DD_EVAL] Text/is_empty expects Text argument");
                }
            }
            // Text/empty() -> ""
            "empty" => Value::text(""),
            // Text/space() -> " "
            "space" => Value::text(" "),
            _ => panic!("[DD_EVAL] Unknown Text function '{}'", name),
        }
    }

    /// Evaluate a pipe expression.
    fn eval_pipe(&mut self, from: &Value, to: &Spanned<Expression>) -> Value {
        match &to.node {
            // Pipe to function call
            Expression::FunctionCall { path, arguments } => {
                let full_path: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                let (namespace, name) = if full_path.len() >= 2 {
                    (Some(full_path[0]), full_path[1])
                } else if full_path.len() == 1 {
                    (None, full_path[0])
                } else {
                    panic!("[DD_EVAL] Bug: empty function path in pipe");
                };

                // List operations use raw expressions (template bindings like `item`
                // can't be evaluated as variables). Skip arg evaluation for them.
                let skip_arg_eval = matches!(
                    (namespace, name),
                    (Some("List"), "remove") | (Some("List"), "clear") | (Some("List"), "retain") | (Some("List"), "map")
                );

                // Build args (only for functions that need evaluated values)
                let args: HashMap<&str, Value> = if skip_arg_eval {
                    HashMap::new()
                } else {
                    arguments
                        .iter()
                        .filter_map(|arg| {
                            let name = arg.node.name.as_str();
                            let value = arg.node.value.as_ref().map(|v| self.eval_expression(&v.node))?;
                            Some((name, value))
                        })
                        .collect()
                };

                match (namespace, name) {
                    (Some("Document"), "new") => {
                        // from |> Document/new() means from is the root
                        if !args.contains_key("root") {
                            return from.clone();
                        }
                        args.get("root").cloned().unwrap_or_else(|| {
                            panic!("[DD_EVAL] Document/new missing root after args check");
                        })
                    }
                    (Some("Math"), "sum") => {
                        // LatestRef |> Math/sum() - create a reactive CellRef for accumulation
                        // DD handles event merging natively
                        // If input is already a CellRef, it's a reactive accumulator
                        if let Value::CellRef(cell_id) = from {
                            if let Some(bindings) = self.latest_cells.get(&cell_id.name()).cloned() {
                                if bindings.is_empty() {
                                    panic!("[DD_EVAL] Math/sum on LATEST has no bindings (all SKIP)");
                                }
                                let sum_cell_id = self.generate_sum_cell_id();
                                self.add_cell_config(CellConfig {
                                    id: CellId::new(&sum_cell_id),
                                    initial: Value::int(0),
                                    triggered_by: Vec::new(),
                                    timer_interval_ms: 0,
                                    filter: EventFilter::Any,
                                    transform: StateTransform::Identity,
                                    persist: true,
                                });

                                for binding in &bindings {
                                    let add_action = match &binding.action {
                                        LinkAction::SetValue(v) => {
                                            if !matches!(v, Value::Number(_)) {
                                                panic!(
                                                    "[DD_EVAL] Math/sum expects numeric LATEST values, found {:?}",
                                                    v
                                                );
                                            }
                                            LinkAction::AddValue(v.clone())
                                        }
                                        other => {
                                            panic!(
                                                "[DD_EVAL] Math/sum only supports numeric LATEST SetValue bindings, found {:?}",
                                                other
                                            );
                                        }
                                    };
                                    let mapping = if let Some(keys) = &binding.key_filter {
                                        LinkCellMapping::with_key_filter(
                                            binding.link_id.clone(),
                                            sum_cell_id.clone(),
                                            add_action,
                                            keys.clone(),
                                        )
                                    } else {
                                        LinkCellMapping::new(
                                            binding.link_id.clone(),
                                            sum_cell_id.clone(),
                                            add_action,
                                        )
                                    };
                                    self.dataflow_config.add_link_mapping(mapping);
                                }

                                return Value::CellRef(CellId::new(sum_cell_id));
                            }
                            panic!(
                                "[DD_EVAL] Math/sum expects LATEST event stream or TimerRef, found CellRef {}",
                                cell_id.name()
                            );
                        }
                        // TimerRef |> Math/sum() - also creates a reactive accumulator
                        if let Value::TimerRef { interval_ms, .. } = from {
                            let cell_id = "timer_counter";
                            // NOTE: Do NOT call init_cell here!
                            // The test expects empty output until the first timer fires.
                            dd_log!("[DD_EVAL] TimerRef |> Math/sum(): {} @ {}ms", cell_id, interval_ms);
                            return Value::TimerRef {
                                id: Arc::from(cell_id),
                                interval_ms: *interval_ms,
                            };
                        }
                        // Static value - just pass through
                        from.clone()
                    }
                    (Some("Router"), "go_to") => {
                        // Register input cell so worker triggers navigation on update
                        if let Value::CellRef(cell_id) = from {
                            dd_log!("[DD_EVAL] Router/go_to(): registering go_to cell '{}'", cell_id.name());
                            self.dataflow_config.add_go_to_cell(cell_id.name());
                        }
                        Value::Unit
                    }
                    (Some("Timer"), "interval") => {
                        // Duration |> Timer/interval() - returns TimerRef
                        let interval_ms = duration_to_millis(from);
                        let timer_id = format!("timer_{}", interval_ms);
                        dd_log!("[DD_EVAL] Timer/interval: {}ms -> {}", interval_ms, timer_id);
                        Value::timer_ref(timer_id, interval_ms)
                    }
                    (Some("Stream"), "skip") => {
                        // from |> Stream/skip(count: n) - in static eval, just return from
                        // (all iterations already done in HOLD)
                        from.clone()
                    }
                    (Some("Log"), "info") => {
                        // from |> Log/info(...) - pass through for static eval
                        from.clone()
                    }
                    // List operations - for static eval, these pass through the list
                    // (reactive operations like append/remove depend on events)
                    (Some("List"), "append") => {
                        // from |> List/append(item: ...) - pass through for static eval
                        // The append operation depends on events (e.g., Enter key press)
                        // so we don't add items during static evaluation.
                        // Initial items come from the LIST literal.
                        if !matches!(from, Value::CellRef(_) | Value::List(_)) {
                            panic!("[DD_EVAL] List/append expects list-like input, found {:?}", from);
                        }
                        from.clone()
                    }
                    (Some("List"), "clear") => {
                        // from |> List/clear(on: ...) - pass through for static eval
                        // The clear operation depends on events (e.g., button press)
                        // so we don't clear items during static evaluation.
                        //
                        // List/clear bindings are parsed from Boon code during evaluation.
                        if !matches!(from, Value::CellRef(_) | Value::List(_)) {
                            panic!("[DD_EVAL] List/clear expects list-like input, found {:?}", from);
                        }
                        from.clone()
                    }
                    (Some("List"), "remove") => {
                        // from |> List/remove(item, on: ...) - parse the on: argument to extract the LinkRef path
                        // E.g., List/remove(item, on: item.todo_elements.remove_todo_button.event.press)
                        // We need to extract ["todo_elements", "remove_todo_button"] as the path

                        // Get the binding name (first argument without value, e.g., "item")
                        let binding_name = arguments
                            .iter()
                            .find(|arg| arg.node.value.is_none())
                            .map(|arg| arg.node.name.as_str());

                        // Get the "on:" event expression (unevaluated)
                        let on_expr = arguments
                            .iter()
                            .find(|arg| arg.node.name.as_str() == "on")
                            .and_then(|arg| arg.node.value.as_ref());

                        let binding = binding_name.unwrap_or_else(|| {
                            panic!("[DD_EVAL] List/remove requires item binding");
                        });
                        let event_expr = on_expr.unwrap_or_else(|| {
                            panic!("[DD_EVAL] List/remove requires on: argument");
                        });
                        let list_cell_id = match &from {
                            Value::CellRef(cell_id) => cell_id.name().to_string(),
                            Value::List(handle) => {
                                handle.cell_id.as_deref().map(|id| id.to_string()).unwrap_or_else(|| {
                                    panic!(
                                        "[DD_EVAL] List/remove requires Collection with cell_id; list literals must be stored in a HOLD before remove"
                                    );
                                })
                            }
                            other => {
                                panic!("[DD_EVAL] List/remove expects list-like input, found {:?}", other);
                            }
                        };
                        self.register_list_remove_binding(&list_cell_id, binding, &event_expr.node);

                        // Pass through for static eval (removal depends on events)
                        if !matches!(from, Value::CellRef(_) | Value::List(_)) {
                            panic!("[DD_EVAL] List/remove expects list-like input, found {:?}", from);
                        }
                        from.clone()
                    }
                    (Some("List"), "retain") => {
                        // from |> List/retain(item, if: ...) - filter items based on predicate
                        // Uses DD-native collection filter

                        // Get the binding name (first argument, usually "item")
                        let binding_name = arguments
                            .iter()
                            .find(|arg| arg.node.value.is_none())
                            .map(|arg| arg.node.name.as_str());

                        // Get the "if:" predicate expression (unevaluated)
                        let predicate_expr = arguments
                            .iter()
                            .find(|arg| arg.node.name.as_str() == "if")
                            .and_then(|arg| arg.node.value.as_ref());
                        let binding = binding_name.unwrap_or_else(|| {
                            panic!("[DD_EVAL] List/retain requires item binding");
                        });
                        let pred_expr = predicate_expr.unwrap_or_else(|| {
                            panic!("[DD_EVAL] List/retain requires if: predicate");
                        });

                        // Handle CellRef input - create DD-native filtered collection
                        if let Value::CellRef(cell_id) = from {
                            // Try to extract field access pattern: `item.field`
                            if let Some((field_name, filter_value)) =
                                self.extract_field_filter(binding, &pred_expr.node)
                            {
                                // Use DD-native filter
                                return self.create_filtered_collection(
                                    &cell_id.name(),
                                    Arc::from(field_name),
                                    filter_value,
                                );
                            }
                            // Complex predicate: create predicate template with Placeholder
                            // Evaluate the predicate with item = Placeholder to create a template
                            let mut template_runtime = self.fork();
                            template_runtime.variables.insert(binding.to_string(), Value::Placeholder);
                            let predicate_template = TemplateValue::from_value(
                                template_runtime.eval_expression(&pred_expr.node)
                            );
                            dd_log!("[DD_EVAL] Phase 4: Complex predicate filter on '{}', template={:?}",
                                cell_id.name(), predicate_template);
                            let source_id = self
                                .get_collection_id(&cell_id.name())
                                .unwrap_or_else(|| self.register_list_hold(&cell_id.name(), Vec::new()));
                            let output_id = self.dataflow_config.add_filter(
                                source_id,
                                None,
                                Some(predicate_template),
                            );
                            return self.collection_value(output_id);
                        }

                        // Handle Collection input - chain filters
                        if let Value::List(handle) = from {
                            if let Some((field_name, filter_value)) =
                                self.extract_field_filter(binding, &pred_expr.node)
                            {
                                return self.chain_filter_on_collection(
                                    handle.id.clone(),
                                    Some((Arc::from(field_name), filter_value)),
                                    None,
                                );
                            }
                            let mut template_runtime = self.fork();
                            template_runtime.variables.insert(binding.to_string(), Value::Placeholder);
                            let predicate_template = TemplateValue::from_value(
                                template_runtime.eval_expression(&pred_expr.node)
                            );
                            dd_log!("[DD_EVAL] Phase 4: Complex predicate filter on Collection {:?}, template={:?}",
                                handle.id, predicate_template);
                            return self.chain_filter_on_collection(
                                handle.id.clone(),
                                None,
                                Some(predicate_template),
                            );
                        }

                        panic!("[DD_EVAL] List/retain expects Collection/CellRef input (list literals are Collections)");
                    }
                    (Some("List"), "map") => {
                        // from |> List/map(item, new: ...) - transform items
                        // Get the binding name (first argument without a value)
                        let binding_name = arguments
                            .iter()
                            .find(|arg| arg.node.value.is_none())
                            .map(|arg| arg.node.name.as_str());

                        // Get the "new:" transform expression (unevaluated)
                        let transform_expr = arguments
                            .iter()
                            .find(|arg| arg.node.name.as_str() == "new")
                            .and_then(|arg| arg.node.value.as_ref());
                        let binding = binding_name.unwrap_or_else(|| {
                            panic!("[DD_EVAL] List/map requires item binding");
                        });
                        let new_expr = transform_expr.unwrap_or_else(|| {
                            panic!("[DD_EVAL] List/map requires new: argument");
                        });

                        match from {
                            // CellRef |> List/map(item, new: ...) -> DD mapped collection
                            // Registers a map operation in the DD dataflow graph
                            Value::CellRef(cell_id) => {
                                // Evaluate the transform with Placeholder as the item
                                // Using fork_eval_merge to preserve collections created in the Map body
                                // (e.g., nested list literals for Row items)
                                let binding_owned = binding.to_string();
                                let element_template = TemplateValue::from_value(
                                    self.fork_eval_merge(
                                        |scoped| { scoped.variables.insert(binding_owned, Value::Placeholder); },
                                        &new_expr.node,
                                    )
                                );
                                dd_log!("[DD_EVAL] List/map on CellRef: source={}, template={:?}", cell_id, element_template);

                                // Use DD-native mapped collection
                                self.create_mapped_collection(&cell_id.name(), element_template)
                            }
                            // Collection |> List/map(item, new: ...) -> chained DD map
                            // This handles filter+map chains: Collection(filtered) |> List/map()
                            Value::List(handle) => {
                                // Evaluate the transform with Placeholder as the item
                                // Using fork_eval_merge to preserve collections created in the Map body
                                let binding_owned = binding.to_string();
                                let element_template = TemplateValue::from_value(
                                    self.fork_eval_merge(
                                        |scoped| { scoped.variables.insert(binding_owned, Value::Placeholder); },
                                        &new_expr.node,
                                    )
                                );
                                dd_log!("[DD_EVAL] List/map on Collection: source={:?}, template={:?}",
                                    handle.id, element_template);

                                // Chain map operation on existing collection (e.g., filtered collection)
                                self.chain_map_on_collection(handle.id.clone(), element_template)
                            }
                            other => panic!("[DD_EVAL] List/map expects list-like input, found {:?}", other),
                        }
                    }
                    (Some("List"), "count") => {
                        // from |> List/count() - DD-native count operation
                        match from {
                            // CellRef |> List/count() -> DD-native count
                            Value::CellRef(cell_id) => {
                                dd_log!("[DD_EVAL] List/count() on CellRef: {}", cell_id);
                                self.create_list_count(&cell_id.name())
                            }
                            // Collection |> List/count() -> chain count on collection
                            // Handles chained operations like filter+count
                            Value::List(handle) => {
                                dd_log!("[DD_EVAL] List/count() on Collection: {:?}", handle.id);
                                self.chain_count_on_collection(handle.id.clone())
                            }
                            other => panic!("[DD_EVAL] List/count expects list-like input, found {:?}", other),
                        }
                    }
                    (Some("List"), "is_empty") => {
                        // from |> List/is_empty() - DD-native is_empty operation
                        match from {
                            // CellRef |> List/is_empty() -> DD-native is_empty
                            Value::CellRef(cell_id) => {
                                dd_log!("[DD_EVAL] List/is_empty() on CellRef: cell_id={}", cell_id);
                                self.create_list_is_empty(&cell_id.name())
                            }
                            // Collection |> List/is_empty() -> chain is_empty on collection
                            Value::List(handle) => {
                                dd_log!("[DD_EVAL] List/is_empty() on Collection: {:?}", handle.id);
                                self.chain_is_empty_on_collection(handle.id.clone())
                            }
                            other => panic!("[DD_EVAL] List/is_empty expects list-like input, found {:?}", other),
                        }
                    }
                    // Bool operations
                    (Some("Bool"), "or") => {
                        // from |> Bool/or(that: other_bool)
                        // Uses short-circuit algebra for reactive operands:
                        //   True OR x = True,  False OR x = x
                        let that_value = args.get("that").unwrap_or_else(|| {
                            panic!("[DD_EVAL] Bool/or requires 'that' argument");
                        });
                        match (self.try_as_bool(from), self.try_as_bool(that_value)) {
                            (Some(a), Some(b)) => Value::Bool(a || b),
                            (Some(true), None) | (None, Some(true)) => Value::Bool(true),
                            (Some(false), None) => that_value.clone(),
                            (None, Some(false)) => from.clone(),
                            (None, None) => {
                                dd_log!("[DD_EVAL] Bool/or: both operands reactive, from={:?}, that={:?}", from, that_value);
                                from.clone()
                            }
                        }
                    }
                    (Some("Bool"), "and") => {
                        // from |> Bool/and(that: other_bool)
                        // Uses short-circuit algebra for reactive operands:
                        //   False AND x = False,  True AND x = x
                        let that_value = args.get("that").unwrap_or_else(|| {
                            panic!("[DD_EVAL] Bool/and requires 'that' argument");
                        });
                        match (self.try_as_bool(from), self.try_as_bool(that_value)) {
                            (Some(a), Some(b)) => Value::Bool(a && b),
                            (Some(false), None) | (None, Some(false)) => Value::Bool(false),
                            (Some(true), None) => that_value.clone(),
                            (None, Some(true)) => from.clone(),
                            (None, None) => {
                                dd_log!("[DD_EVAL] Bool/and: both operands reactive, from={:?}, that={:?}", from, that_value);
                                from.clone()
                            }
                        }
                    }
                    (Some("Bool"), "not") => {
                        // from |> Bool/not()
                        // During template evaluation, placeholders must be deferred
                        match from {
                            Value::Placeholder | Value::PlaceholderField(_) | Value::PlaceholderBoolNot(_) => {
                                Value::PlaceholderBoolNot(Box::new(from.clone()))
                            }
                            _ => {
                                let from_bool = self.require_bool(from, "Bool/not input");
                                Value::Bool(!from_bool)
                            }
                        }
                    }
                    // Text functions (piped)
                    (Some("Text"), "trim") => {
                        // from |> Text/trim()
                        if let Value::Text(s) = from {
                            Value::text(s.trim())
                        } else {
                            panic!("[DD_EVAL] Text/trim expects Text input, found {:?}", from);
                        }
                    }
                    (Some("Text"), "is_not_empty") => {
                        // from |> Text/is_not_empty()
                        if let Value::Text(s) = from {
                            Value::Bool(!s.is_empty())
                        } else {
                            panic!("[DD_EVAL] Text/is_not_empty expects Text input, found {:?}", from);
                        }
                    }
                    (Some("Text"), "is_empty") => {
                        // from |> Text/is_empty()
                        if let Value::Text(s) = from {
                            Value::Bool(s.is_empty())
                        } else {
                            panic!("[DD_EVAL] Text/is_empty expects Text input, found {:?}", from);
                        }
                    }
                    // User-defined function: pass piped value as first argument
                    (None, func_name) => {
                        self.eval_user_function_with_piped(func_name, from, &args)
                    }
                    _ => panic!("[DD_EVAL] Unsupported pipe function: {:?}::{}", namespace, name),
                }
            }

            // Pipe to HOLD - iterate if body contains Stream/pulses
            Expression::Hold { state_param, body } => {
                self.eval_hold(from, state_param.as_str(), &body.node, to.persistence.as_ref())
            }

            // Pipe to THEN - return unit (needs event trigger)
            Expression::Then { .. } => Value::Unit,

            // Pipe to WHEN - pattern match and return body
            Expression::When { arms } => self.eval_pattern_match(from, arms),

            // Pipe to WHILE - pattern match and return body (same as WHEN for static)
            Expression::While { arms } => self.eval_pattern_match(from, arms),

            // Pipe to field access
            Expression::FieldAccess { path } => {
                let mut current = from.clone();
                for field in path {
                    // Handle Placeholder specially - create Tagged placeholder for field access
                    // Pure DD: Use PlaceholderField value for later template substitution
                    current = match &current {
                        Value::Placeholder => {
                            Value::PlaceholderField(Arc::new(vec![Arc::from(field.as_str())]))
                        }
                        Value::PlaceholderField(path) => {
                            let mut new_path = (**path).clone();
                            new_path.push(Arc::from(field.as_str()));
                            Value::PlaceholderField(Arc::new(new_path))
                        }
                        // Handle LinkRef.event - create synthetic event object with all event types
                        Value::LinkRef(link_id) if field.as_str() == "event" => {
                            Self::build_link_event_object(link_id)
                        }
                        // Handle Element.event - navigate through nested element.event path
                        Value::Tagged { tag, fields, .. }
                            if tag.as_ref() == "Element" && field.as_str() == "event" =>
                        {
                            fields
                                .get("element")
                                .and_then(|e| e.get("event"))
                                .cloned()
                                .unwrap_or(Value::Unit)
                        }
                        // Forward references are Unit during pass 1; propagate Unit.
                        Value::Unit => Value::Unit,
                        _ => current.get(field.as_str()).cloned().unwrap_or_else(|| {
                            panic!("[DD_EVAL] Bug: missing field '{}' on {:?}", field, current);
                        }),
                    };
                }
                current
            }

            // Pipe to LINK { alias } - replace internal LinkRef with the target from alias
            // In Boon, `element |> LINK { store.nav.home }`:
            // 1. Evaluates the alias to get the stored LinkRef (e.g., link_1)
            // 2. Finds any LinkRef in the element and replaces it with the stored one
            // 3. Returns the modified element
            Expression::LinkSetter { alias } => {
                // Get the target LinkRef from the alias
                let target_link = self.eval_alias(&alias.node);
                dd_log!("[DD_EVAL] LinkSetter: alias={:?} -> target_link={:?}", alias.node, target_link);

                // Replace any LinkRef in the element with the target
                let result = if let Value::LinkRef(target_id) = &target_link {
                    let result = self.replace_link_ref_in_value(from, target_id);
                    dd_log!("[DD_EVAL] LinkSetter: replaced LinkRef with {}", target_id);
                    result
                } else if let Value::PlaceholderField(path) = &target_link {
                    // Template evaluation: replace LinkRef with placeholder field value
                    // During cloning/substitution, this will be resolved to the real LinkRef from the data item
                    let result = self.replace_link_ref_with_placeholder(from, path);
                    dd_log!("[DD_EVAL] LinkSetter: replaced LinkRef with placeholder field {:?}", path);
                    result
                } else {
                    // If alias doesn't resolve to a LinkRef or PlaceholderField, just pass through unchanged
                    dd_log!("[DD_EVAL] LinkSetter: alias did not resolve to LinkRef, passing through unchanged");
                    from.clone()
                };

                result
            }

            // Pipe to Alias — check if it's a function name (e.g., `value |> value_container`)
            Expression::Alias(crate::parser::static_expression::Alias::WithoutPassed { parts, .. }) => {
                if parts.len() == 1 {
                    let name = parts[0].as_str();
                    if self.functions.contains_key(name) {
                        let args = HashMap::new();
                        return self.eval_user_function_with_piped(name, from, &args);
                    }
                }
                // Not a function — evaluate as normal expression
                self.eval_expression(&to.node)
            }

            // Default
            _ => self.eval_expression(&to.node),
        }
    }

    /// Evaluate a HOLD expression with initial value and body.
    ///
    /// For static evaluation, HOLD iterates if the body contains Stream/pulses.
    /// This enables fibonacci-style computations:
    ///   [prev: 0, curr: 1] |> HOLD state {
    ///     n-1 |> Stream/pulses() |> THEN { [prev: state.curr, curr: state.prev + state.curr] }
    ///   }
    fn eval_hold(
        &mut self,
        initial: &Value,
        state_name: &str,
        body: &Expression,
        hold_persistence: Option<&Persistence>,
    ) -> Value {
        // Try to extract pulse count from body: `count |> Stream/pulses() |> ...`
        let pulse_count = self.extract_pulse_count(body);

        if pulse_count == 0 {
            // Check if body contains a timer trigger (variable that evaluates to TimerRef)
            // e.g., `tick |> THEN { state + 1 }` where tick = Duration |> Timer/interval()
            if let Some(interval_ms) = self.extract_timer_trigger_from_body(body) {
                let cell_id = "timer_counter";
                // NOTE: Do NOT call init_cell here - test expects empty until first tick
                dd_log!("[DD_EVAL] Timer-triggered HOLD detected: {} @ {}ms", cell_id, interval_ms);

                // Task 4.4: Build CellConfig during evaluation (not in interpreter)
                self.add_cell_config(CellConfig {
                    id: CellId::new(cell_id),
                    initial: Value::Unit, // Unit = "not yet rendered" until first timer tick
                    triggered_by: Vec::new(), // Timer-triggered, no external triggers
                    timer_interval_ms: interval_ms,
                    filter: EventFilter::Any,
                    transform: StateTransform::Increment,
                    persist: false, // Timer values are NOT persisted
                });

                // Return TimerRef so interpreter sets up timer-triggered HOLD
                return Value::TimerRef {
                    id: Arc::from(cell_id),
                    interval_ms,
                };
            }

            // Check if body contains a LINK trigger (reactive HOLD)
            // e.g., `button.event.press |> THEN { state + 1 }`
            if self.contains_link_trigger(body) {
                // Generate unique HOLD ID for this HOLD instance
                let cell_id = self.generate_cell_id(hold_persistence);

                // For boolean HOLDs, extract link actions from the body (set true/false or toggle).
                // Note: In Boon, True/False are tags (Tagged { tag: "False" }), not native bools.
                let is_boolean_hold = matches!(initial, Value::Bool(_))
                    || matches!(initial, Value::Tagged { tag, .. } if BoolTag::is_bool_tag(tag.as_ref()));
                let mut has_link_actions = false;
                if is_boolean_hold {
                    dd_log!("[DD_EVAL] is_boolean_hold=true for {}, extracting link actions", cell_id);
                    let bindings = self.extract_bool_set_bindings_with_link_ids(body);
                    for binding in bindings {
                        let mapping = if let Some(keys) = binding.key_filter {
                            LinkCellMapping::with_key_filter(
                                binding.link_id,
                                cell_id.clone(),
                                binding.action.clone(),
                                keys,
                            )
                        } else {
                            LinkCellMapping::new(
                                binding.link_id,
                                cell_id.clone(),
                                binding.action.clone(),
                            )
                        };
                        self.dataflow_config.add_link_mapping(mapping);
                        has_link_actions = true;
                    }

                    // Also extract toggle event bindings (click |> THEN { state |> Bool/not() })
                    let toggle_bindings = self.extract_toggle_bindings_with_link_ids(body, state_name);
                    for (_event_path, _event_type, link_id) in toggle_bindings {
                        let Some(link_id) = link_id else {
                            continue; // pass 1: forward ref
                        };
                        self.dataflow_config.add_link_mapping(LinkCellMapping::bool_toggle(
                            link_id,
                            cell_id.clone(),
                        ));
                        has_link_actions = true;
                    }
                }

                // Global/cross-cell toggle patterns are not supported in pure DD mode.
                // These now fail fast inside determine_transform_from_then_body.

                dd_log!("[DD_EVAL] LINK-triggered HOLD detected: {} with initial {:?}", cell_id, initial);

                // Task 4.4: Build CellConfig during evaluation
                // Determine the transform from the body pattern
                let mut transform = self.determine_transform(body, state_name);
                dd_log!("[DD_EVAL] Determined transform: {:?}", transform);

                // Extract trigger LinkId from body dynamically (no hardcoded fallbacks)
                let mut triggered_by = if has_link_actions {
                    Vec::new()
                } else if let Some(latest_mappings) = self.extract_hold_latest_link_mappings(body, state_name) {
                    // LATEST with per-input arithmetic transforms (e.g., +1 / -1)
                    dd_log!("[DD_EVAL] LATEST HOLD with {} per-input link mappings for {}", latest_mappings.len(), cell_id);
                    for (link_id, action) in &latest_mappings {
                        self.dataflow_config.add_link_mapping(LinkCellMapping::new(
                            link_id.clone(),
                            cell_id.clone(),
                            action.clone(),
                        ));
                    }
                    has_link_actions = true;
                    transform = StateTransform::Identity;
                    Vec::new()
                } else if let Some(id) = self.extract_link_trigger_id(body) {
                    vec![LinkId::new(&id)]
                } else {
                    // Complex body (e.g., LATEST with link triggers wrapping THEN).
                    // The inner LATEST already registers its own link mappings and cell config.
                    // Use empty triggers — the HOLD cell holds its initial value and
                    // relies on link actions to update it directly.
                    dd_log!("[DD_EVAL] Complex HOLD body for {} — using empty triggers (LATEST handles links)", cell_id);
                    transform = StateTransform::Identity;
                    Vec::new()
                };
                dd_log!("[DD_EVAL] CellConfig triggered_by: {:?}", triggered_by);
                if has_link_actions {
                    transform = StateTransform::Identity;
                    triggered_by = Vec::new();
                }

                // Add CellConfig with dynamically extracted triggers
                let triggered_by_clone = triggered_by.clone();
                let transform_clone = transform.clone();
                self.add_cell_config(CellConfig {
                    id: CellId::new(&cell_id),
                    initial: initial.clone(),
                    triggered_by,
                    timer_interval_ms: 0,
                    filter: EventFilter::Any,
                    transform,
                    persist: hold_persistence.is_some(),
                });

                // Register link mappings for SetTrue/SetFalse transforms so template cloning can remap them.
                for link_id in &triggered_by_clone {
                    match transform_clone {
                        StateTransform::SetTrue => {
                            self.dataflow_config.add_link_mapping(
                                LinkCellMapping::set_true(link_id.name().to_string(), cell_id.clone()),
                            );
                        }
                        StateTransform::SetFalse => {
                            self.dataflow_config.add_link_mapping(
                                LinkCellMapping::set_false(link_id.name().to_string(), cell_id.clone()),
                            );
                        }
                        _ => {}
                    }
                }

                // Return CellRef - bridge will render reactively
                return Value::CellRef(CellId::new(cell_id));
            }

            // No Stream/pulses and no LINK trigger, just return initial value
            return initial.clone();
        }

        // Extract the THEN body (what to compute on each pulse)
        let then_body = self.extract_then_body(body);

        let Some(then_body) = then_body else {
            // No THEN body found, return initial
            return initial.clone();
        };

        // Iterate pulse_count times, accumulating state
        let mut current_state = initial.clone();

        for _ in 0..pulse_count {
            // Create runtime with state bound to current value
            let mut iter_runtime = self.fork();
            iter_runtime.variables.insert(state_name.to_string(), current_state.clone());

            // Evaluate the THEN body to get next state
            let next_state = iter_runtime.eval_expression(then_body);

            // Skip Unit results (like SKIP in WHEN patterns)
            if next_state != Value::Unit {
                current_state = next_state;
            }
        }

        current_state
    }

    /// Extract pulse count from expressions like `n |> Stream/pulses()`.
    /// Handles nested pipes: `n - 1 |> Stream/pulses() |> THEN { ... }`
    fn extract_pulse_count(&mut self, expr: &Expression) -> i64 {
        match expr {
            Expression::Pipe { from, to } => {
                // Check if `to` is Stream/pulses: `count |> Stream/pulses()`
                if self.is_stream_pulses(&to.node) {
                    if let Value::Number(n) = self.eval_expression(&from.node) {
                        let count = n.0.trunc();
                        if count < 0.0 { return 0; }
                        #[allow(clippy::cast_possible_truncation)]
                        return if count < i64::MAX as f64 { count as i64 } else {
                            panic!("[DD_EVAL] Pulse count {} too large", count)
                        };
                    }
                }
                // Recurse into BOTH sides of the pipe
                // This handles: `(n-1 |> Stream/pulses()) |> THEN { ... }`
                let from_count = self.extract_pulse_count(&from.node);
                if from_count > 0 {
                    return from_count;
                }
                self.extract_pulse_count(&to.node)
            }
            _ => 0,
        }
    }

    /// Check if expression is Stream/pulses().
    fn is_stream_pulses(&self, expr: &Expression) -> bool {
        if let Expression::FunctionCall { path, .. } = expr {
            let parts: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
            return parts == vec!["Stream", "pulses"];
        }
        false
    }

    /// Extract the THEN body from a pipe chain.
    fn extract_then_body<'a>(&self, expr: &'a Expression) -> Option<&'a Expression> {
        match expr {
            // ... |> THEN { body }
            Expression::Pipe { to, .. } => {
                if let Expression::Then { body } = &to.node {
                    return Some(&body.node);
                }
                // Recurse
                self.extract_then_body(&to.node)
            }
            Expression::Then { body } => Some(&body.node),
            _ => None,
        }
    }

    /// Register List/remove bindings (per-item or bulk) from an `on:` expression.
    fn register_list_remove_binding(
        &mut self,
        list_cell_id: &str,
        binding: &str,
        event_expr: &Expression,
    ) {
        if let Some(path) = self.extract_linkref_path_from_event(binding, event_expr) {
            dd_log!("[DD_EVAL] List/remove parsed on: binding={}, path={:?}", binding, path);
            self.dataflow_config.set_remove_event_path(list_cell_id, path);
            return;
        }

        if let Some(global_path) = self.extract_global_event_path(event_expr) {
            let link_value = self.resolve_field_path(&global_path).unwrap_or_else(|| {
                panic!(
                    "[DD_EVAL] List/remove bulk on: could not resolve event path {:?}",
                    global_path
                );
            });
            let link_id = match link_value {
                Value::LinkRef(id) => id,
                other => {
                    panic!(
                        "[DD_EVAL] List/remove bulk on: path {:?} did not resolve to LinkRef, found {:?}",
                        global_path, other
                    );
                }
            };
            let (field, value) = self.extract_bulk_remove_filter_from_event(binding, event_expr)
                .unwrap_or_else(|| {
                    panic!(
                        "[DD_EVAL] List/remove bulk requires explicit predicate (e.g., item.completed)"
                    );
                });
            self.dataflow_config.add_bulk_remove_binding(
                link_id,
                list_cell_id,
                Some((Arc::from(field), value)),
                None,
            );
            return;
        }

        panic!(
            "[DD_EVAL] List/remove on: could not extract event path for binding '{}'",
            binding
        );
    }

    /// Extract the bulk remove filter from a List/remove event expression.
    /// Expects `... |> THEN { item.field |> WHEN { True => [], False => SKIP } }`.
    fn extract_bulk_remove_filter_from_event(
        &mut self,
        binding: &str,
        event_expr: &Expression,
    ) -> Option<(String, Value)> {
        let then_body = self.extract_then_body(event_expr)?;
        self.extract_bulk_remove_filter_from_body(binding, then_body)
    }

    fn extract_bulk_remove_filter_from_body(
        &mut self,
        binding: &str,
        body: &Expression,
    ) -> Option<(String, Value)> {
        match body {
            Expression::Pipe { from, to } if matches!(to.node, Expression::When { .. }) => {
                self.extract_field_filter(binding, &from.node)
            }
            Expression::Block { output, .. } => {
                self.extract_bulk_remove_filter_from_body(binding, &output.node)
            }
            _ => self.extract_field_filter(binding, body),
        }
    }

    /// Extract the path to a LinkRef from an event expression.
    /// Handles patterns like `item.todo_elements.remove_todo_button.event.press`
    /// Returns the path between binding and "event": ["todo_elements", "remove_todo_button"]
    fn extract_linkref_path_from_event(
        &self,
        binding: &str,
        event_expr: &Expression,
    ) -> Option<Vec<String>> {
        // The expression is typically an Alias like: item.todo_elements.remove_todo_button.event.press
        // We need to extract the path between the binding and "event"
        match event_expr {
            Expression::Alias(Alias::WithoutPassed { parts, .. }) => {
                // Find "event" in the path
                if let Some(event_idx) = parts.iter().position(|p| p.as_ref() == "event") {
                    // Check that it starts with the binding
                    if parts.first().map(|p| p.as_ref()) == Some(binding) {
                        // Extract path between binding (index 0) and "event" (event_idx)
                        // E.g., ["item", "todo_elements", "remove_todo_button", "event", "press"]
                        //         0       1                2                     3       4
                        // We want [1..3] = ["todo_elements", "remove_todo_button"]
                        if event_idx > 1 {
                            let path: Vec<String> = parts[1..event_idx]
                                .iter()
                                .map(|p| p.to_string())
                                .collect();
                            return Some(path);
                        }
                    }
                }
                None
            }
            Expression::FieldAccess { path } => {
                // Similar logic for FieldAccess
                if let Some(event_idx) = path.iter().position(|p| p.as_ref() == "event") {
                    if path.first().map(|p| p.as_ref()) == Some(binding) && event_idx > 1 {
                        let result: Vec<String> = path[1..event_idx]
                            .iter()
                            .map(|p| p.to_string())
                            .collect();
                        return Some(result);
                    }
                }
                None
            }
            Expression::Pipe { from, .. } => {
                // The from side might be the event path
                self.extract_linkref_path_from_event(binding, &from.node)
            }
            _ => None,
        }
    }

    /// Extract a global event path (not starting with binding).
    /// Handles patterns like `elements.remove_completed_button.event.press`
    /// Returns the full path up to "event": ["elements", "remove_completed_button"]
    fn extract_global_event_path(&self, event_expr: &Expression) -> Option<Vec<String>> {
        match event_expr {
            Expression::Alias(Alias::WithoutPassed { parts, .. }) => {
                // Find "event" in the path
                if let Some(event_idx) = parts.iter().position(|p| p.as_ref() == "event") {
                    if event_idx > 0 {
                        let path: Vec<String> = parts[..event_idx]
                            .iter()
                            .map(|p| p.to_string())
                            .collect();
                        return Some(path);
                    }
                }
                None
            }
            Expression::FieldAccess { path } => {
                if let Some(event_idx) = path.iter().position(|p| p.as_ref() == "event") {
                    if event_idx > 0 {
                        let result: Vec<String> = path[..event_idx]
                            .iter()
                            .map(|p| p.to_string())
                            .collect();
                        return Some(result);
                    }
                }
                None
            }
            Expression::Pipe { from, .. } => {
                // The from side might be the event path
                self.extract_global_event_path(&from.node)
            }
            _ => None,
        }
    }

    /// Extract field filter pattern from predicate expression.
    /// Handles patterns like `item.completed` where `item` is the binding name.
    /// Returns (field_name, filter_value) if pattern matches.
    fn extract_field_filter(
        &mut self,
        binding: &str,
        predicate: &Expression,
    ) -> Option<(String, Value)> {
        // Boon uses Tagged booleans (Tagged { tag: "True" }) not Rust bools (Bool(true))
        // This must match how initial values and toggle results are stored
        let tagged_true = Value::Tagged {
            tag: Arc::from("True"),
            fields: Arc::new(BTreeMap::new()),
        };
        let tagged_false = Value::Tagged {
            tag: Arc::from("False"),
            fields: Arc::new(BTreeMap::new()),
        };

        match predicate {
            // Pattern: `item.field` - field access is the entire predicate
            // Path is [item, field], meaning: filter where field == true
            Expression::FieldAccess { path } => {
                if path.len() == 2 && path[0].as_ref() == binding {
                    return Some((path[1].to_string(), tagged_true));
                }
                None
            }
            // Pattern: `item.field` as Alias (parser produces this for variable.field)
            Expression::Alias(Alias::WithoutPassed { parts, .. }) => {
                if parts.len() == 2 && parts[0].as_ref() == binding {
                    return Some((parts[1].to_string(), tagged_true));
                }
                None
            }
            // Pattern: `item.field |> Bool/not()` - negated field access
            Expression::Pipe { from, to } => {
                if let Expression::FunctionCall { path: fn_path, .. } = &to.node {
                    if fn_path.len() == 2 && fn_path[0] == "Bool" && fn_path[1] == "not" {
                        // Check for FieldAccess
                        if let Expression::FieldAccess { path } = &from.node {
                            if path.len() == 2 && path[0].as_ref() == binding {
                                return Some((path[1].to_string(), tagged_false.clone()));
                            }
                        }
                        // Check for Alias
                        if let Expression::Alias(Alias::WithoutPassed { parts, .. }) = &from.node {
                            if parts.len() == 2 && parts[0].as_ref() == binding {
                                return Some((parts[1].to_string(), tagged_false));
                            }
                        }
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Extract boolean set bindings with actual LinkRef IDs by evaluating the event source expressions.
    ///
    /// Patterns supported:
    /// - `path.event.X |> THEN { True/False }`
    /// - `path.event.key_down.key |> WHEN { Enter => False }`
    fn extract_bool_set_bindings_with_link_ids(&mut self, body: &Expression) -> Vec<BoolLinkBinding> {
        let mut bindings = Vec::new();

        // Helper to extract inputs from LATEST
        let inputs = match body {
            Expression::Latest { inputs, .. } => inputs.iter().map(|s| &s.node).collect::<Vec<_>>(),
            _ => vec![body],
        };

        for input in inputs {
            if let Expression::Pipe { from, to } = input {
                match &to.node {
                    Expression::Then { body: then_body } => {
                        if let Some(result) = self.extract_bool_literal(&then_body.node) {
                            let Some(link_id) = self.resolve_link_id_from_event_source(&from.node) else {
                                continue; // pass 1: forward ref
                            };
                            let action = if result { LinkAction::SetTrue } else { LinkAction::SetFalse };
                            bindings.push(BoolLinkBinding {
                                link_id,
                                action,
                                key_filter: None,
                            });
                        }
                    }
                    Expression::When { arms } => {
                        let mut true_keys: Vec<Key> = Vec::new();
                        let mut false_keys: Vec<Key> = Vec::new();
                        for arm in arms {
                            if let Pattern::Literal(Literal::Tag(name)) = &arm.pattern {
                                if let Some(result) = self.extract_bool_literal(&arm.body.node) {
                                    if result {
                                        true_keys.push(Key::from_str(name.as_ref()));
                                    } else {
                                        false_keys.push(Key::from_str(name.as_ref()));
                                    }
                                }
                            }
                        }

                        if !true_keys.is_empty() || !false_keys.is_empty() {
                            let Some(link_id) = self.resolve_link_id_from_event_source(&from.node) else {
                                continue; // pass 1: forward ref
                            };
                            if !true_keys.is_empty() {
                                bindings.push(BoolLinkBinding {
                                    link_id: link_id.clone(),
                                    action: LinkAction::SetTrue,
                                    key_filter: Some(true_keys),
                                });
                            }
                            if !false_keys.is_empty() {
                                bindings.push(BoolLinkBinding {
                                    link_id,
                                    action: LinkAction::SetFalse,
                                    key_filter: Some(false_keys),
                                });
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        bindings
    }

    fn resolve_link_id_from_event_source(&mut self, from: &Expression) -> Option<String> {
        if let Some((path, _event_type)) = self.extract_event_path(from) {
            if let Some(val) = self.resolve_field_path(&path) {
                if let Value::LinkRef(id) = val {
                    return Some(id.to_string());
                }
            }
        }
        let from_value = self.eval_expression(from);
        if let Value::LinkRef(id) = from_value {
            return Some(id.to_string());
        }
        None
    }

    fn resolve_link_id_from_event_text(&mut self, expr: &Expression) -> Option<String> {
        match expr {
            Expression::Alias(Alias::WithoutPassed { parts, .. }) => {
                if parts.last().map(|p| p.as_ref()) != Some("text") || parts.len() < 2 {
                    return None;
                }
                let path: Vec<String> = parts[..parts.len() - 1].iter().map(|p| p.to_string()).collect();
                self.resolve_field_path(&path).and_then(|val| {
                    if let Value::LinkRef(id) = val {
                        Some(id.to_string())
                    } else {
                        None
                    }
                })
            }
            Expression::FieldAccess { path } => {
                if path.last().map(|p| p.as_ref()) != Some("text") || path.len() < 2 {
                    return None;
                }
                let result: Vec<String> = path[..path.len() - 1].iter().map(|p| p.to_string()).collect();
                self.resolve_field_path(&result).and_then(|val| {
                    if let Value::LinkRef(id) = val {
                        Some(id.to_string())
                    } else {
                        None
                    }
                })
            }
            Expression::Pipe { from, .. } => self.resolve_link_id_from_event_text(&from.node),
            _ => None,
        }
    }

    /// Extract the path and event type from an expression like `todo_elements.todo_title_element.event.double_click`.
    /// Returns (["todo_elements", "todo_title_element"], "double_click").
    fn extract_event_path(&self, expr: &Expression) -> Option<(Vec<String>, String)> {
        match expr {
            Expression::Alias(Alias::WithoutPassed { parts, .. }) => {
                // Find "event" in the path, extract path before it and event type after
                if let Some(event_idx) = parts.iter().position(|p| p.as_ref() == "event") {
                    if event_idx > 0 && event_idx + 1 < parts.len() {
                        let path: Vec<String> = parts[..event_idx].iter().map(|p| p.to_string()).collect();
                        let event_type = parts[event_idx + 1].to_string();
                        return Some((path, event_type));
                    }
                }
                None
            }
            Expression::FieldAccess { path } => {
                if let Some(event_idx) = path.iter().position(|p| p.as_ref() == "event") {
                    if event_idx > 0 && event_idx + 1 < path.len() {
                        let result: Vec<String> = path[..event_idx].iter().map(|p| p.to_string()).collect();
                        let event_type = path[event_idx + 1].to_string();
                        return Some((result, event_type));
                    }
                }
                None
            }
            Expression::Pipe { from, .. } => {
                // Recurse into the from side (event path might be piped)
                self.extract_event_path(&from.node)
            }
            _ => None,
        }
    }

    /// Extract a boolean literal from an expression.
    fn extract_bool_literal(&self, expr: &Expression) -> Option<bool> {
        match expr {
            Expression::Literal(Literal::Tag(s)) => {
                match s.as_str() {
                    "True" => Some(true),
                    "False" => Some(false),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Resolve a field path like ["todo_elements", "editing_todo_title_element", "event", "key_down"]
    /// to its Value by traversing the current scope.
    fn resolve_field_path(&self, path: &[String]) -> Option<Value> {
        if path.is_empty() {
            return None;
        }
        if path.iter().any(|part| part.as_str() == ITEM_KEY_FIELD) {
            panic!(
                "[DD_EVAL] '{}' is internal; Boon code must not access it",
                ITEM_KEY_FIELD
            );
        }
        if let Some(part) = path.iter().find(|part| part.as_str().starts_with("__")) {
            panic!(
                "[DD_EVAL] '{}' is internal; Boon code must not access reserved fields",
                part
            );
        }
        // Start by looking up the first part in variables
        let first = &path[0];
        if let Some(root) = self.variables.get(first.as_str()) {
            let mut current = root.clone();
            // Traverse the remaining path — if traversal fails (e.g., stale value from
            // a prior pass), fall through to the self-reference fallback below.
            let mut ok = true;
            for (i, part) in path[1..].iter().enumerate() {
                // Handle Placeholder types: build PlaceholderField for remaining path
                match &current {
                    Value::Placeholder => {
                        let remaining: Vec<Arc<str>> = path[1 + i..].iter().map(|p| Arc::from(p.as_str())).collect();
                        return Some(Value::PlaceholderField(Arc::new(remaining)));
                    }
                    Value::PlaceholderField(existing) => {
                        let mut extended = existing.as_ref().clone();
                        for p in &path[1 + i..] {
                            extended.push(Arc::from(p.as_str()));
                        }
                        return Some(Value::PlaceholderField(Arc::new(extended)));
                    }
                    _ => {}
                }
                if let Some(next) = current.get(part.as_str()) {
                    current = next.clone();
                } else {
                    ok = false;
                    break;
                }
            }
            if ok {
                return Some(current);
            }
        }
        // Self-reference fallback: when inside an object like `store: [input: LINK, ...]`,
        // a reference to `store.input.event.key_down` has path ["store", "input", ...].
        // The "store" variable from a prior evaluation pass may be Unit or partial,
        // so traversal above fails. But "input" is available as a scoped variable
        // (added during the current object evaluation). Try resolving without the first element.
        if path.len() > 1 {
            return self.resolve_field_path(&path[1..]);
        }
        None
    }

    /// Check if expression is a Bool/not() toggle pattern like `state |> Bool/not()`.
    /// Returns true if the expression is a boolean toggle on the HOLD state.
    fn is_bool_toggle_pattern(&self, expr: &Expression, state_name: &str) -> bool {
        match expr {
            Expression::Pipe { from, to } => {
                // Check: state |> Bool/not()
                if let Expression::FunctionCall { path, .. } = &to.node {
                    let fn_path: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                    if fn_path == vec!["Bool", "not"] {
                        // Check if from is the state variable (can be Variable or Alias)
                        match &from.node {
                            Expression::Variable(v) => {
                                return v.name.as_ref() == state_name;
                            }
                            Expression::Alias(Alias::WithoutPassed { parts, .. }) => {
                                // Single-part alias like just "state"
                                if parts.len() == 1 && parts[0].as_ref() == state_name {
                                    return true;
                                }
                            }
                            Expression::Alias(Alias::WithPassed { extra_parts, .. }) => {
                                // PASSED.state - treat as the same state variable
                                if extra_parts.len() == 1 && extra_parts[0].as_ref() == state_name {
                                    return true;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                false
            }
            _ => false,
        }
    }

    /// Extract toggle bindings with actual LinkRef IDs by evaluating the event source.
    ///
    /// This method EVALUATES the `from` expression in toggle patterns to get actual LinkRef IDs.
    /// Used for Task 4.3: eliminate interpreter's extract_checkbox_toggles dependency.
    ///
    /// Returns: Vec<(event_path, event_type, Option<link_id>)>
    fn extract_toggle_bindings_with_link_ids(&mut self, body: &Expression, state_name: &str) -> Vec<(Vec<String>, String, Option<String>)> {
        let mut toggles = Vec::new();

        // Helper to extract inputs from LATEST
        let inputs = match body {
            Expression::Latest { inputs, .. } => {
                inputs.iter().map(|s| &s.node).collect::<Vec<_>>()
            }
            _ => vec![body],
        };

        for input in inputs.iter() {
            // Look for patterns like: path.event.click |> THEN { state |> Bool/not() }
            if let Expression::Pipe { from, to } = input {
                if let Expression::Then { body: then_body } = &to.node {
                    // Check if THEN body is Bool/not() toggle
                    if self.is_bool_toggle_pattern(&then_body.node, state_name) {
                        // Extract the event path from the from side (for logging/fallback)
                        let path_info = self.extract_event_path(&from.node);

                        // CRITICAL: Evaluate the `from` expression to get actual LinkRef ID
                        let from_value = self.eval_expression(&from.node);
                        let link_id = if let Value::LinkRef(id) = from_value {
                            dd_log!("[DD_EVAL] extract_toggle_bindings_with_link_ids: evaluated to LinkRef({})", id);
                            Some(id.to_string())
                        } else {
                            None
                        };

                        let Some((path, event_type)) = path_info else {
                            continue; // pass 1: forward ref
                        };
                        let Some(link_id) = link_id else {
                            continue; // pass 1: forward ref
                        };
                        toggles.push((path, event_type, Some(link_id)));
                    }
                }
            }
        }

        toggles
    }

    /// Check if expression contains a LINK trigger (reactive event source).
    /// Used to detect LINK-triggered HOLDs like:
    ///   `button.event.press |> THEN { state + 1 }`
    ///
    /// Detection strategy: Look for the `X |> THEN { ... }` pattern which is
    /// the canonical event-driven reactive pattern in Boon. If the HOLD body
    /// pipes something to THEN, it's event-driven and should return CellRef.
    fn contains_link_trigger(&self, expr: &Expression) -> bool {
        match expr {
            Expression::Pipe { from: _, to } => {
                // Check if piping to THEN - this is the event-driven pattern
                if matches!(to.node, Expression::Then { .. }) {
                    return true;
                }
                // Recursively check both sides of the pipe
                self.contains_link_trigger(&to.node)
            }
            Expression::Then { .. } => {
                // THEN by itself indicates event-driven behavior
                true
            }
            Expression::When { arms } | Expression::While { arms } => {
                arms.iter().any(|arm| self.contains_link_trigger(&arm.body.node))
            }
            Expression::Latest { inputs, .. } => {
                // Check inside LATEST - common pattern: LATEST { event |> THEN { ... } }
                inputs.iter().any(|item| self.contains_link_trigger(&item.node))
            }
            _ => false,
        }
    }

    /// Extract the LinkRef ID from a LINK trigger expression.
    /// Used to populate `triggered_by` in CellConfig for non-boolean HOLDs.
    /// Pattern: `link_expr |> THEN { ... }` → extracts LinkRef ID from link_expr
    ///
    /// Task 7.1: This enables dynamic trigger IDs instead of hardcoded fallbacks.
    fn extract_link_trigger_id(&mut self, expr: &Expression) -> Option<String> {
        match expr {
            Expression::Pipe { from, to } => {
                // Pattern: X |> THEN { ... } - evaluate X to get LinkRef
                if matches!(to.node, Expression::Then { .. }) {
                    let from_value = self.eval_expression(&from.node);
                    if let Value::LinkRef(id) = from_value {
                        dd_log!("[DD_EVAL] extract_link_trigger_id: found LinkRef({})", id);
                        return Some(id.to_string());
                    }
                }
                // Recursively check the to side
                self.extract_link_trigger_id(&to.node)
            }
            Expression::Latest { inputs, .. } => {
                // Check inside LATEST - return first found trigger
                for item in inputs {
                    if let Some(id) = self.extract_link_trigger_id(&item.node) {
                        return Some(id);
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Check if expression contains a timer trigger (Timer/interval).
    /// Used to detect timer-triggered patterns.
    fn contains_timer_trigger(&self, expr: &Expression) -> bool {
        match expr {
            Expression::Pipe { from, to } => {
                // Check if piping FROM Timer/interval
                if let Expression::FunctionCall { path, .. } = &from.node {
                    let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                    if path_strs == ["Timer", "interval"] {
                        return true;
                    }
                }
                // Check if piping TO Timer/interval
                if let Expression::FunctionCall { path, .. } = &to.node {
                    let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                    if path_strs == ["Timer", "interval"] {
                        return true;
                    }
                }
                // Recursively check both sides
                self.contains_timer_trigger(&from.node) || self.contains_timer_trigger(&to.node)
            }
            Expression::FunctionCall { path, .. } => {
                let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                path_strs == ["Timer", "interval"]
            }
            _ => false,
        }
    }

    /// Extract timer info (interval_ms) from an expression containing Timer/interval.
    fn extract_timer_info(&mut self, expr: &Expression) -> Option<(String, u64)> {
        match expr {
            Expression::Pipe { from, to } => {
                // Duration |> Timer/interval()
                if let Expression::FunctionCall { path, .. } = &to.node {
                    let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                    if path_strs == ["Timer", "interval"] {
                        // Evaluate the Duration
                        let duration = self.eval_expression(&from.node);
                        let interval_ms = duration_to_millis(&duration);
                        let timer_id = format!("timer_{}", interval_ms);
                        return Some((timer_id, interval_ms));
                    }
                }
                // Recursively check
                self.extract_timer_info(&from.node)
                    .or_else(|| self.extract_timer_info(&to.node))
            }
            _ => None,
        }
    }

    // Task 6.3: is_latest_sum_pattern DELETED - dead code (never called)

    /// Check if expression contains a THEN pattern (for LATEST+sum detection).
    fn contains_then_pattern(&self, expr: &Expression) -> bool {
        match expr {
            Expression::Then { .. } => true,
            Expression::Pipe { from: _, to } => {
                matches!(to.node, Expression::Then { .. }) || self.contains_then_pattern(&to.node)
            }
            // Check inside LATEST inputs for THEN patterns
            Expression::Latest { inputs } => {
                inputs.iter().any(|input| self.contains_then_pattern(&input.node))
            }
            _ => false,
        }
    }

    fn is_event_text_path_parts(parts: &[crate::parser::StrSlice]) -> bool {
        if parts.len() < 3 {
            return false;
        }
        if parts.last().map(|p| p.as_ref()) != Some("text") {
            return false;
        }
        parts.iter().position(|p| p.as_ref() == "event")
            .map(|event_idx| event_idx + 1 < parts.len())
            .unwrap_or(false)
    }

    fn is_event_text_path(&self, expr: &Expression) -> bool {
        match expr {
            Expression::Alias(Alias::WithoutPassed { parts, .. }) => Self::is_event_text_path_parts(parts),
            Expression::FieldAccess { path } => Self::is_event_text_path_parts(path),
            Expression::Pipe { from, .. } => self.is_event_text_path(&from.node),
            _ => false,
        }
    }

    fn extract_latest_when_bindings(
        &mut self,
        link_id: &str,
        arms: &[Arm],
    ) -> Vec<LatestLinkBinding> {
        let mut bindings = Vec::new();
        for arm in arms {
            match &arm.pattern {
                Pattern::Literal(Literal::Tag(tag)) => {
                    if matches!(arm.body.node, Expression::Skip) {
                        continue;
                    }
                    let value = self.eval_expression(&arm.body.node);
                    if value.is_list_like() {
                        panic!(
                            "[DD_EVAL] LATEST WHEN cannot SetValue to list for link '{}' (use list diffs)",
                            link_id
                        );
                    }
                    bindings.push(LatestLinkBinding {
                        link_id: link_id.to_string(),
                        action: LinkAction::SetValue(value),
                        key_filter: Some(vec![Key::from_str(tag.as_ref())]),
                    });
                }
                Pattern::WildCard => {
                    if matches!(arm.body.node, Expression::Skip) {
                        continue;
                    }
                    panic!(
                        "[DD_EVAL] LATEST WHEN default value is not supported; use SKIP for '__' to avoid ambiguous matches"
                    );
                }
                other => {
                    panic!(
                        "[DD_EVAL] LATEST WHEN only supports key tag patterns, found {:?}",
                        other
                    );
                }
            }
        }
        bindings
    }

    fn extract_latest_event_bindings(&mut self, expr: &Expression) -> Option<Vec<LatestLinkBinding>> {
        if let Expression::Pipe { from, to } = expr {
            if let Expression::Then { body } = &to.node {
                let Some(link_id) = self.resolve_link_id_from_event_source(&from.node) else {
                    // During pass 1, forward-referenced LINKs evaluate to Unit.
                    // Skip binding extraction; pass 2 will resolve correctly.
                    return None;
                };
                if matches!(body.node, Expression::Skip) {
                    return Some(Vec::new());
                }
                let value = self.eval_expression(&body.node);
                if value.is_list_like() {
                    panic!(
                        "[DD_EVAL] LATEST THEN cannot SetValue to list for link '{}' (use list diffs)",
                        link_id
                    );
                }
                return Some(vec![LatestLinkBinding {
                    link_id,
                    action: LinkAction::SetValue(value),
                    key_filter: None,
                }]);
            }
            if let Expression::When { arms } = &to.node {
                let Some(link_id) = self.resolve_link_id_from_event_source(&from.node) else {
                    return None; // pass 1: forward ref
                };
                return Some(self.extract_latest_when_bindings(&link_id, arms));
            }
        }

        if self.is_event_text_path(expr) {
            let Some(link_id) = self.resolve_link_id_from_event_text(expr) else {
                return None; // pass 1: forward ref
            };
            return Some(vec![LatestLinkBinding {
                link_id,
                action: LinkAction::SetText,
                key_filter: None,
            }]);
        }

        None
    }

    // Task 6.3: get_latest_initial DELETED - dead code (never called)

    /// Check if expression is the Timer + Math/sum pattern.
    /// Pattern: `Duration |> Timer/interval() |> THEN { value } |> Math/sum()`
    fn is_timer_sum_pattern(&self, from: &Expression, to: &Expression) -> bool {
        // Check if `to` is Math/sum()
        let is_math_sum = match to {
            Expression::FunctionCall { path, arguments: _ } => {
                let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                path_strs == ["Math", "sum"]
            }
            _ => false,
        };

        if !is_math_sum {
            return false;
        }

        // Check if `from` contains Timer/interval and THEN
        self.contains_timer_trigger(from) && self.contains_then_pattern(from)
    }

    /// Extract timer trigger from HOLD body.
    ///
    /// Detects patterns like `tick |> THEN { ... }` where `tick` is a variable
    /// that evaluates to a TimerRef. Returns the interval_ms if found.
    fn extract_timer_trigger_from_body(&mut self, body: &Expression) -> Option<u64> {
        match body {
            Expression::Pipe { from, to } => {
                // Check if piping TO THEN
                if matches!(to.node, Expression::Then { .. }) {
                    // Evaluate the FROM side to see if it's a TimerRef
                    let from_val = self.eval_expression(&from.node);
                    if let Value::TimerRef { interval_ms, .. } = from_val {
                        return Some(interval_ms);
                    }
                }
                // Recurse into nested pipes
                self.extract_timer_trigger_from_body(&to.node)
            }
            _ => None,
        }
    }

    // Task 6.3: is_latest_router_pattern DELETED - dead code (never called)

    /// Extract link→route mappings from LATEST expression.
    ///
    /// For patterns like:
    /// ```boon
    /// LATEST {
    ///     nav.home.event.press |> THEN { TEXT { / } }
    ///     nav.about.event.press |> THEN { TEXT { /about } }
    /// }
    /// ```
    ///
    /// The path `nav.home.event.press` references a LINK where:
    /// - `nav.home` is the LinkRef (e.g., "link_1")
    /// - `.event.press` is the event path (stripped)
    ///
    /// We extract `nav.home` → evaluate to LinkRef("link_1") → map to route.
    /// Evaluate LATEST expression - merge multiple reactive inputs.
    ///
    /// LATEST { a, b, c } semantics:
    /// - Static inputs (no THEN) are initial values
    /// - Event-driven inputs (X |> THEN { Y }) are reactive triggers
    /// - Returns LatestRef if any inputs are event-driven
    /// - Returns first non-Unit value if all inputs are static
    fn eval_latest(&mut self, inputs: &[Spanned<Expression>]) -> Value {
        let mut initial_value = Value::Unit;
        let mut has_events = false;
        let mut bindings: Vec<LatestLinkBinding> = Vec::new();
        let mut collection_inputs: Vec<LatestCollectionInput> = Vec::new();

        for input in inputs {
            if let Some(mut latest_bindings) = self.extract_latest_event_bindings(&input.node) {
                has_events = true;
                bindings.append(&mut latest_bindings);
                continue;
            }

            if self.contains_then_pattern(&input.node) {
                // During pass 1, event sources may be Unit (forward reference).
                // extract_latest_event_bindings returned None because LinkRef couldn't
                // be resolved. Mark as event-driven and skip; pass 2 will resolve.
                has_events = true;
                continue;
            }

            if self.extract_event_path(&input.node).is_some() {
                // Same as above: unresolved event path during pass 1.
                has_events = true;
                continue;
            }

            // Static input - evaluate and use as initial if we haven't found one yet
            let val = self.eval_expression(&input.node);
            if let Some(collection_input) = self.extract_latest_collection_input(&val) {
                if initial_value != Value::Unit {
                    panic!("[DD_EVAL] LATEST cannot mix collections with scalar values");
                }
                collection_inputs.push(collection_input);
                continue;
            }
            if !collection_inputs.is_empty() {
                if val != Value::Unit {
                    panic!("[DD_EVAL] LATEST cannot mix collections with scalar values");
                }
                continue;
            }
            if initial_value == Value::Unit && val != Value::Unit {
                initial_value = val;
            }
        }

        let collection_count = collection_inputs.len();

        if has_events {
            if collection_count > 0 {
                panic!(
                    "[DD_EVAL] LATEST cannot mix event-driven inputs with collections; use list diffs"
                );
            }
        } else if collection_count > 0 {
            if collection_count == 1 {
                let input = collection_inputs.pop().unwrap_or_else(|| {
                    panic!("[DD_EVAL] LATEST expected collection input");
                });
                if let Some(cell_id) = input.cell_id {
                    self.cell_to_collection.insert(input.id.name(), input.id);
                    return Value::List(CollectionHandle::with_id_and_cell(input.id, cell_id));
                }
                return self.collection_value(input.id);
            }
            let mut concat_ids: Vec<CollectionId> = Vec::new();
            for input in &collection_inputs {
                concat_ids.push(input.id);
            }
            let mut iter = concat_ids.into_iter();
            let first = iter.next().unwrap_or_else(|| {
                panic!("[DD_EVAL] LATEST concat requires at least one collection input");
            });
            let output_id = iter.fold(first, |left, right| self.chain_concat_on_collection(left, right));
            return self.collection_value(output_id);
        } else if !has_events {
            // All static - return first non-Unit value (current behavior)
            return initial_value;
        }

        if bindings.is_empty() {
            // During pass 1, all event sources may be unresolved forward references.
            // Return Unit; pass 2 will resolve them and produce valid bindings.
            return Value::Unit;
        }

        if initial_value.is_list_like() {
            panic!("[DD_EVAL] LATEST with list defaults is not supported; use list diffs");
        }

        let cell_id = self.generate_latest_cell_id();
        self.add_cell_config(CellConfig {
            id: CellId::new(&cell_id),
            initial: initial_value,
            triggered_by: Vec::new(),
            timer_interval_ms: 0,
            filter: EventFilter::Any,
            transform: StateTransform::Identity,
            persist: false,
        });

        self.latest_cells.insert(cell_id.clone(), bindings.clone());

        for binding in bindings {
            let mapping = if let Some(keys) = binding.key_filter {
                LinkCellMapping::with_key_filter(
                    binding.link_id,
                    cell_id.clone(),
                    binding.action,
                    keys,
                )
            } else {
                LinkCellMapping::new(
                    binding.link_id,
                    cell_id.clone(),
                    binding.action,
                )
            };
            self.dataflow_config.add_link_mapping(mapping);
        }

        Value::CellRef(CellId::new(cell_id))
    }

    /// Evaluate pattern matching for WHEN/WHILE.
    fn eval_pattern_match(&mut self, value: &Value, arms: &[Arm]) -> Value {
        // Debug: log what value type is being pattern matched
        dd_log!("[DD_EVAL] eval_pattern_match input: {:?}", value);

        // If input is a CellRef, handle reactively
        if let Value::CellRef(cell_id) = value {
            let mut evaluated_arms = Vec::new();
            let mut default_value = None;
            let mut is_alias_only = false;

            for arm in arms {
                let pattern_value = self.pattern_to_value(&arm.pattern);

                if matches!(arm.pattern, Pattern::WildCard) {
                    let body_result = self.fork_eval_merge(|_| {}, &arm.body.node);
                    default_value = Some(Arc::new(body_result));
                } else if let Pattern::Alias { name } = &arm.pattern {
                    // Alias pattern - bind the CellRef to the alias name
                    let alias_value = value.clone();
                    let alias_name = name.to_string();
                    dd_log!("[DD_EVAL] CellRef WHEN: binding '{}' to CellRef for body evaluation", alias_name);
                    let body_result = self.fork_eval_merge(|rt| {
                        rt.variables.insert(alias_name, alias_value);
                    }, &arm.body.node);
                    default_value = Some(Arc::new(body_result));
                    is_alias_only = true;
                } else if let Some(pv) = pattern_value {
                    let body_result = self.fork_eval_merge(|_| {}, &arm.body.node);
                    evaluated_arms.push((pv, body_result));
                }
            }

            // Optimization: alias-only WHEN (catch-all) on CellRef — return body directly.
            // No WhileConfig needed since there's no conditional logic.
            if is_alias_only && evaluated_arms.is_empty() {
                if let Some(body) = default_value {
                    dd_log!("[DD_EVAL] CellRef WHEN: alias-only, returning body directly");
                    return (*body).clone();
                }
            }

            // Check if all arms produce DD-scalar values (Text, Number, Bool, Unit).
            // If so, use ScalarWhen DD operator for efficient dataflow processing.
            let all_scalar = !evaluated_arms.is_empty()
                && evaluated_arms.iter().all(|(_, body)| is_dd_scalar(body))
                && default_value.as_ref().map_or(true, |d| is_dd_scalar(d));

            if all_scalar {
                if let Some(source_col) = self.try_get_collection_id_for_cellref(cell_id) {
                    let scalar_arms: Vec<(Value, Value)> = evaluated_arms;
                    let default = default_value.map(|v| (*v).clone()).unwrap_or(Value::Unit);
                    dd_log!("[DD_EVAL] CellRef WHEN: creating ScalarWhen with {} arms", scalar_arms.len());
                    return self.create_scalar_when(source_col, scalar_arms, default);
                }
            }

            // Fallback: WhileConfig for bridge rendering (element-producing arms
            // or when source lacks CollectionId)
            dd_log!("[DD_EVAL] Created WHILE config for hold {} with {} arms", cell_id, evaluated_arms.len());

            let arms_list: Vec<WhileArm> = evaluated_arms
                .into_iter()
                .map(|(pattern, body)| WhileArm { pattern, body })
                .collect();

            return Value::WhileConfig(Arc::new(WhileConfig {
                cell_id: cell_id.clone(),
                arms: Arc::new(arms_list),
                default: Box::new(default_value.map(|v| (*v).clone()).unwrap_or(Value::Unit)),
            }));
        }

        // If input is a placeholder field value, create placeholder WHILE config
        // This handles: todo.editing |> WHILE { True => ..., False => ... } in templates
        if let Value::PlaceholderField(path) = value {

            // Pre-evaluate all arms for later substitution
            // Note: WHILE_PREEVAL_DEPTH hack was removed - fine-grained signals
            // from cell_signal() prevent spurious side effects during pre-evaluation
            let mut evaluated_arms = Vec::new();
            let mut default_value = None;

            for arm in arms {
                let pattern_value = self.pattern_to_value(&arm.pattern);

                if matches!(arm.pattern, Pattern::WildCard) {
                    let body_result = self.fork_eval_merge(|_| {}, &arm.body.node);
                    default_value = Some(Arc::new(body_result));
                } else if let Some(pv) = pattern_value {
                    let body_result = self.fork_eval_merge(|_| {}, &arm.body.node);
                    evaluated_arms.push((pv, body_result));
                }
            }

            dd_log!("[DD_EVAL] Created placeholder WHILE config for path with {} arms", evaluated_arms.len());

            let arms_list: Vec<WhileArm> = evaluated_arms
                .into_iter()
                .map(|(pattern, body)| WhileArm { pattern, body })
                .collect();

            return Value::PlaceholderWhile(Arc::new(PlaceholderWhileConfig {
                field_path: path.clone(),
                arms: Arc::new(arms_list),
                default: Box::new(default_value.map(|v| (*v).clone()).unwrap_or(Value::Unit)),
            }));
        }

        // NOTE: ComputedRef handling was removed in pure DD migration
        // Boolean computations should go through DD operators instead

        // If input is a LinkRef, check the pattern type:
        // - Boolean arms (True/False) → hover state: element.hovered |> WHILE { True => ..., False => ... }
        // - Key/tag arms (Enter, Escape, etc.) → event filter: event.key_down.key |> WHEN { Enter => ... }
        if let Value::LinkRef(link_id) = value {
            // Non-boolean patterns (key events, etc.) are event filters handled at the AST level.
            // Return the LinkRef so downstream code can track the event source.
            let is_boolean_hover = arms.iter().any(|arm| {
                matches!(&arm.pattern, Pattern::Literal(Literal::Tag(t))
                    if t.as_ref() == "True" || t.as_ref() == "False")
            });
            if !is_boolean_hover {
                return value.clone();
            }
            // Reuse existing hover mapping if present; otherwise allocate a fresh cell id.
            let existing_hover_cell = self.dataflow_config.link_mappings.iter().find_map(|mapping| {
                if mapping.link_id == *link_id && matches!(mapping.action, LinkAction::HoverState) {
                    Some(mapping.cell_id.name())
                } else {
                    None
                }
            });
            let cell_id = if let Some(existing) = existing_hover_cell {
                existing
            } else {
                let new_cell_id = format!("hover_{}", link_id.name());
                // Register initial state and link mapping for hover updates
                self.dataflow_config.add_cell_initialization(&new_cell_id, Value::Bool(false), false);
                self.dataflow_config.add_link_mapping(LinkCellMapping::hover_state(link_id.clone(), new_cell_id.clone()));
                new_cell_id
            };

            // Pre-evaluate all arms for the bridge to render reactively
            let mut evaluated_arms = Vec::new();
            let mut default_value = None;

            for arm in arms {
                let pattern_value = self.pattern_to_value(&arm.pattern);

                if matches!(arm.pattern, Pattern::WildCard) {
                    let body_result = self.fork_eval_merge(|_| {}, &arm.body.node);
                    default_value = Some(Arc::new(body_result));
                } else if let Some(pv) = pattern_value {
                    let body_result = self.fork_eval_merge(|_| {}, &arm.body.node);
                    evaluated_arms.push((pv, body_result));
                }
            }

            dd_log!("[DD_EVAL] Created WHILE config for LinkRef {} (hover hold: {}) with {} arms", link_id, cell_id, evaluated_arms.len());

            let arms_list: Vec<WhileArm> = evaluated_arms
                .into_iter()
                .map(|(pattern, body)| WhileArm { pattern, body })
                .collect();

            return Value::WhileConfig(Arc::new(WhileConfig {
                cell_id: CellId::new(&cell_id),
                arms: Arc::new(arms_list),
                default: Box::new(default_value.map(|v| (*v).clone()).unwrap_or(Value::Unit)),
            }));
        }

        // If input is a WHILE config, chain the pattern matching
        // This happens when: route |> WHEN { "/" => Home } |> WHILE { Home => page(...) }
        if let Value::WhileConfig(config) = value {
            let cell_id = config.cell_id.clone();
            let input_arms = config.arms.clone();
            let input_default = if *config.default != Value::Unit {
                Some(config.default.clone())
            } else {
                None
            };
            // For each arm in this WHILE, evaluate the body for each possible input value
            // This creates a composed transformation: route → page tag → page element
            let mut evaluated_arms = Vec::new();
            let mut default_value = None;

            // For each input arm (e.g., "/" → Home), find the corresponding output
            for arm in input_arms.iter() {
                let input_pattern = &arm.pattern;
                let intermediate_value = &arm.body;
                // Match the intermediate value against this WHILE's patterns
                for arm in arms {
                    let pattern_value = self.pattern_to_value(&arm.pattern);

                    if matches!(arm.pattern, Pattern::WildCard) {
                        continue; // Handle wildcard separately
                    }

                    if let Some(pv) = &pattern_value {
                        // Check if the intermediate value matches this pattern
                        let matches = match (intermediate_value, pv) {
                            (Value::Tagged { tag: a, .. }, Value::Tagged { tag: b, .. }) => a == b,
                            (Value::Text(a), Value::Text(b)) => a == b,
                            _ => intermediate_value == pv,
                        };

                        if matches {
                            // Evaluate the body and map from input pattern to body result
                            let body_result = self.fork_eval_merge(|_| {}, &arm.body.node);
                            evaluated_arms.push((input_pattern.clone(), body_result));
                            break;
                        }
                    }
                }
            }

            // Handle default case from input WhileRef
            if let Some(input_def) = &input_default {
                // Find matching arm for the default intermediate value
                for arm in arms {
                    if matches!(arm.pattern, Pattern::WildCard) {
                        let body_result = self.fork_eval_merge(|_| {}, &arm.body.node);
                        default_value = Some(Arc::new(body_result));
                        break;
                    }

                    let pattern_value = self.pattern_to_value(&arm.pattern);
                    if let Some(pv) = &pattern_value {
                        let matches = match (&**input_def, pv) {
                            (Value::Tagged { tag: a, .. }, Value::Tagged { tag: b, .. }) => a == b,
                            _ => &**input_def == pv,
                        };
                        if matches {
                            let body_result = self.fork_eval_merge(|_| {}, &arm.body.node);
                            default_value = Some(Arc::new(body_result));
                            break;
                        }
                    }
                }
            }

            // Also handle this WHILE's wildcard as default
            if default_value.is_none() {
                for arm in arms {
                    if matches!(arm.pattern, Pattern::WildCard) {
                        let body_result = self.fork_eval_merge(|_| {}, &arm.body.node);
                        default_value = Some(Arc::new(body_result));
                        break;
                    }
                }
            }

            dd_log!("[DD_EVAL] Chained WHILE config for cell {} with {} arms", cell_id, evaluated_arms.len());

            let arms_list: Vec<WhileArm> = evaluated_arms
                .into_iter()
                .map(|(pattern, body)| WhileArm { pattern, body })
                .collect();

            return Value::WhileConfig(Arc::new(WhileConfig {
                cell_id,
                arms: Arc::new(arms_list),
                default: Box::new(default_value.map(|v| (*v).clone()).unwrap_or(Value::Unit)),
            }));
        }

        // Static evaluation for non-CellRef inputs
        for arm in arms {
            if let Some(bindings) = self.match_pattern(value, &arm.pattern) {
                // Create new runtime with pattern bindings
                return self.fork_eval_merge(|rt| {
                    for (name, bound_value) in bindings {
                        rt.variables.insert(name, bound_value);
                    }
                }, &arm.body.node);
            }
        }
        Value::Unit
    }

    /// Convert a pattern to a Value for WhileRef arms.
    /// Used to pre-evaluate pattern values for reactive matching.
    fn pattern_to_value(&mut self, pattern: &Pattern) -> Option<Value> {
        match pattern {
            Pattern::Literal(lit) => {
                Some(self.eval_literal(lit))
            }
            Pattern::TaggedObject { tag, .. } => {
                Self::assert_tag_not_reserved(tag.as_str(), "tagged pattern");
                // For tag patterns without fields, create a simple tag value
                Some(Value::Tagged {
                    tag: Arc::from(tag.as_str()),
                    fields: Arc::new(BTreeMap::new()),
                })
            }
            Pattern::Alias { name } => {
                // Alias patterns match anything, use the name as identifier
                Some(Value::text(name.as_str()))
            }
            Pattern::WildCard => {
                // Wildcard handled separately as default
                None
            }
            Pattern::Object { .. } | Pattern::List { .. } | Pattern::Map { .. } => {
                panic!("[DD_EVAL] Complex patterns (Object/List/Map) are not supported for reactive WHEN/WHILE");
            }
        }
    }

    /// Try to match a value against a pattern, returning bindings if successful.
    fn match_pattern(&mut self, value: &Value, pattern: &Pattern) -> Option<Vec<(String, Value)>> {
        match pattern {
            Pattern::WildCard => Some(vec![]),

            Pattern::Alias { name } => {
                // Bind the value to the name
                Some(vec![(name.as_str().to_string(), value.clone())])
            }

            Pattern::Literal(lit) => {
                // Special case: Bool values matching True/False tag literals
                if let Value::Bool(b) = value {
                    if let Literal::Tag(tag_name) = lit {
                        let tag_str = tag_name.as_str();
                        if BoolTag::matches_bool(tag_str, *b) {
                            return Some(vec![]);
                        } else if BoolTag::is_bool_tag(tag_str) {
                            return None;
                        }
                    }
                }

                let pattern_value = self.eval_literal(lit);
                if *value == pattern_value {
                    Some(vec![])
                } else {
                    None
                }
            }

            Pattern::TaggedObject { tag, variables } => {
                Self::assert_tag_not_reserved(tag.as_str(), "tagged pattern");
                // Special case: Bool values matching True/False tags
                if let Value::Bool(b) = value {
                    let tag_name = tag.as_str();
                    if BoolTag::matches_bool(tag_name, *b) {
                        return Some(vec![]);
                    } else {
                        return None;
                    }
                }

                if let Value::Tagged { tag: value_tag, fields } = value {
                    if tag.as_str() == value_tag.as_ref() {
                        // Match fields
                        let mut bindings = vec![];
                        for var in variables {
                            let field_value = fields.get(var.name.as_str()).cloned().unwrap_or_else(|| {
                                panic!("[DD_EVAL] Bug: tagged pattern missing field '{}'", var.name);
                            });
                            bindings.push((var.name.as_str().to_string(), field_value));
                        }
                        Some(bindings)
                    } else {
                        None
                    }
                } else {
                    None
                }
            }

            Pattern::Object { variables } => {
                if let Value::Object(fields) = value {
                    let mut bindings = vec![];
                    for var in variables {
                        let field_value = fields.get(var.name.as_str()).cloned().unwrap_or_else(|| {
                            panic!("[DD_EVAL] Bug: object pattern missing field '{}'", var.name);
                        });
                        bindings.push((var.name.as_str().to_string(), field_value));
                    }
                    Some(bindings)
                } else {
                    None
                }
            }

            Pattern::List { items } => {
                match value {
                    Value::List(_) => {
                        panic!("[DD_EVAL] List patterns are not supported for Collection-backed lists");
                    }
                    _ => None,
                }
            }

            Pattern::Map { .. } => {
                // Map patterns not commonly used, return None for now
                None
            }
        }
    }

    /// Evaluate a comparator.
    fn eval_comparator(&mut self, comp: &Comparator) -> Value {
        match comp {
            Comparator::Equal { operand_a, operand_b } => {
                let a = self.eval_expression(&operand_a.node);
                let b = self.eval_expression(&operand_b.node);
                // DD-native equality comparison using DD collection operators
                match (&a, &b) {
                    // Collection == Collection => DD Equal operator
                    // Used for: all_completed: completed_list_count == list_count
                    // DD-native pattern for equality
                    (Value::List(left_handle), Value::List(right_handle)) => {
                        self.create_equal(left_handle.id.clone(), right_handle.id.clone())
                    }
                    // Collection comparisons require explicit DD operators.
                    (Value::List(handle), other) |
                    (other, Value::List(handle)) => {
                        panic!(
                            "[DD_EVAL] Collection comparison requires a DD operator \
                             (collection id: {:?}, other: {:?})",
                            handle.id, other
                        );
                    }
                    _ => {
                        #[cfg(debug_assertions)]
                        dd_log!("[DD_EVAL] Comparing {:?} == {:?} => {:?}", a, b, a == b);
                        Value::Bool(a == b)
                    }
                }
            }
            Comparator::NotEqual { operand_a, operand_b } => {
                let a = self.eval_expression(&operand_a.node);
                let b = self.eval_expression(&operand_b.node);
                if matches!(a, Value::List(_)) || matches!(b, Value::List(_)) {
                    panic!("[DD_EVAL] Collection comparison requires a DD operator: {:?} != {:?}", a, b);
                }
                Value::Bool(a != b)
            }
            Comparator::Less { operand_a, operand_b } => {
                let a = self.eval_expression(&operand_a.node);
                let b = self.eval_expression(&operand_b.node);
                if matches!(a, Value::List(_)) || matches!(b, Value::List(_)) {
                    panic!("[DD_EVAL] Collection comparison requires a DD operator: {:?} < {:?}", a, b);
                }
                Value::Bool(a < b)
            }
            Comparator::Greater { operand_a, operand_b } => {
                let a = self.eval_expression(&operand_a.node);
                let b = self.eval_expression(&operand_b.node);
                match (&a, &b) {
                    // Collection > 0 => DD GreaterThanZero operator
                    // Used for: show_clear_completed: completed_list_count > 0
                    // DD-native pattern for greater-than-zero
                    (Value::List(handle), Value::Number(n)) if n.0 == 0.0 => {
                        self.create_greater_than_zero(handle.id.clone())
                    }
                    (Value::List(_), _) | (_, Value::List(_)) => {
                        panic!("[DD_EVAL] Collection comparison requires a DD operator: {:?} > {:?}", a, b);
                    }
                    _ => Value::Bool(a > b),
                }
            }
            Comparator::LessOrEqual { operand_a, operand_b } => {
                let a = self.eval_expression(&operand_a.node);
                let b = self.eval_expression(&operand_b.node);
                if matches!(a, Value::List(_)) || matches!(b, Value::List(_)) {
                    panic!("[DD_EVAL] Collection comparison requires a DD operator: {:?} <= {:?}", a, b);
                }
                Value::Bool(a <= b)
            }
            Comparator::GreaterOrEqual { operand_a, operand_b } => {
                let a = self.eval_expression(&operand_a.node);
                let b = self.eval_expression(&operand_b.node);
                if matches!(a, Value::List(_)) || matches!(b, Value::List(_)) {
                    panic!("[DD_EVAL] Collection comparison requires a DD operator: {:?} >= {:?}", a, b);
                }
                Value::Bool(a >= b)
            }
        }
    }

    /// Evaluate an arithmetic operator.
    fn eval_arithmetic(&mut self, op: &ArithmeticOperator) -> Value {
        match op {
            ArithmeticOperator::Add { operand_a, operand_b } => {
                let a = self.eval_expression(&operand_a.node);
                let b = self.eval_expression(&operand_b.node);
                match (&a, &b) {
                    (Value::Number(x), Value::Number(y)) => Value::float(x.0 + y.0),
                    (Value::Text(x), Value::Text(y)) => Value::text(format!("{}{}", x, y)),
                    _ => {
                        panic!("[DD_EVAL] Add expects Number+Number or Text+Text, found {:?} + {:?}", a, b);
                    }
                }
            }
            ArithmeticOperator::Subtract { operand_a, operand_b } => {
                let a = self.eval_expression(&operand_a.node);
                let b = self.eval_expression(&operand_b.node);
                dd_log!("[DD_EVAL] Subtract: a={:?}, b={:?}", a, b);
                match (&a, &b) {
                    (Value::Number(x), Value::Number(y)) => Value::float(x.0 - y.0),
                    // Collection - Collection => DD Subtract operator
                    // Used for: active_list_count: list_count - completed_list_count
                    // DD-native pattern for subtraction
                    (Value::List(left_handle), Value::List(right_handle)) => {
                        dd_log!("[DD_EVAL] Creating DD Subtract: {:?} - {:?}", left_handle.id, right_handle.id);
                        self.create_subtract(left_handle.id.clone(), right_handle.id.clone())
                    }
                    // CellRef - CellRef => collection refs from DD operations (e.g., List/filter)
                    (Value::CellRef(left_id), Value::CellRef(right_id)) => {
                        let left_col = self.get_collection_id(&left_id.name()).unwrap_or_else(|| {
                            panic!("[DD_EVAL] Subtract: CellRef '{}' is not a collection", left_id.name());
                        });
                        let right_col = self.get_collection_id(&right_id.name()).unwrap_or_else(|| {
                            panic!("[DD_EVAL] Subtract: CellRef '{}' is not a collection", right_id.name());
                        });
                        dd_log!("[DD_EVAL] Creating DD Subtract from CellRefs: {:?} - {:?}", left_col, right_col);
                        self.create_subtract(left_col, right_col)
                    }
                    // Mixed: List - CellRef
                    (Value::List(handle), Value::CellRef(cell_id)) => {
                        let right_col = self.get_collection_id(&cell_id.name()).unwrap_or_else(|| {
                            panic!("[DD_EVAL] Subtract: CellRef '{}' is not a collection", cell_id.name());
                        });
                        self.create_subtract(handle.id.clone(), right_col)
                    }
                    // Mixed: CellRef - List
                    (Value::CellRef(cell_id), Value::List(handle)) => {
                        let left_col = self.get_collection_id(&cell_id.name()).unwrap_or_else(|| {
                            panic!("[DD_EVAL] Subtract: CellRef '{}' is not a collection", cell_id.name());
                        });
                        self.create_subtract(left_col, handle.id.clone())
                    }
                    _ => {
                        panic!("[DD_EVAL] Subtract expects Number-Number or Collection-Collection, found {:?} - {:?}", a, b);
                    }
                }
            }
            ArithmeticOperator::Multiply { operand_a, operand_b } => {
                let a = self.eval_expression(&operand_a.node);
                let b = self.eval_expression(&operand_b.node);
                match (&a, &b) {
                    (Value::Number(x), Value::Number(y)) => Value::float(x.0 * y.0),
                    _ => {
                        panic!("[DD_EVAL] Multiply expects Number*Number, found {:?} * {:?}", a, b);
                    }
                }
            }
            ArithmeticOperator::Divide { operand_a, operand_b } => {
                let a = self.eval_expression(&operand_a.node);
                let b = self.eval_expression(&operand_b.node);
                match (&a, &b) {
                    (Value::Number(x), Value::Number(y)) if y.0 != 0.0 => Value::float(x.0 / y.0),
                    (Value::Number(_), Value::Number(_)) => {
                        panic!("[DD_EVAL] Divide by zero is not allowed");
                    }
                    _ => {
                        panic!("[DD_EVAL] Divide expects Number/Number, found {:?} / {:?}", a, b);
                    }
                }
            }
            ArithmeticOperator::Negate { operand } => {
                let a = self.eval_expression(&operand.node);
                match &a {
                    Value::Number(x) => Value::float(-x.0),
                    _ => {
                        panic!("[DD_EVAL] Negate expects Number, found {:?}", a);
                    }
                }
            }
        }
    }

    /// Evaluate an alias (variable reference with optional field path).
    fn eval_alias(&mut self, alias: &Alias) -> Value {
        match alias {
            Alias::WithoutPassed { parts, .. } => {
                if parts.is_empty() {
                    panic!("[DD_EVAL] Bug: Alias without parts");
                }

                // First part is the variable name
                let mut current = self
                    .variables
                    .get(parts[0].as_str())
                    .cloned()
                    .unwrap_or_else(|| {
                        panic!("[DD_EVAL] Bug: undefined variable '{}'", parts[0]);
                    });

                // Rest are field accesses
                for field in parts.iter().skip(1) {
                    if field.as_ref() == ITEM_KEY_FIELD {
                        panic!(
                            "[DD_EVAL] '{}' is internal; Boon code must not access it",
                            ITEM_KEY_FIELD
                        );
                    }
                    // Handle Placeholder specially - create PlaceholderField for field access
                    current = match &current {
                        Value::Placeholder => {
                            Value::PlaceholderField(Arc::new(vec![Arc::from(field.as_str())]))
                        }
                        Value::PlaceholderField(path) => {
                            let mut new_path = (**path).clone();
                            new_path.push(Arc::from(field.as_str()));
                            Value::PlaceholderField(Arc::new(new_path))
                        }
                        // Handle LinkRef.event - create synthetic event object with all event types
                        Value::LinkRef(link_id) if field.as_str() == "event" => {
                            Self::build_link_event_object(link_id)
                        }
                        // Handle Element.event - navigate through nested element.event path
                        Value::Tagged { tag, fields, .. }
                            if tag.as_ref() == "Element" && field.as_str() == "event" =>
                        {
                            fields
                                .get("element")
                                .and_then(|e| e.get("event"))
                                .cloned()
                                .unwrap_or(Value::Unit)
                        }
                        // Forward references are Unit during pass 1; propagate Unit.
                        Value::Unit => Value::Unit,
                        _ => current.get(field.as_str()).cloned().unwrap_or_else(|| {
                            panic!("[DD_EVAL] Bug: missing field '{}' on {:?}", field, current);
                        }),
                    };
                }

                current
            }
            Alias::WithPassed { extra_parts } => {
                // PASSED value - access the passed_context and navigate through fields
                let mut current = self.passed_context.clone().unwrap_or_else(|| {
                    panic!("[DD_EVAL] Bug: PASSED used without context");
                });

                // Track list source for PASSED context too
                let mut list_source_name: Option<String> = None;

                // Navigate through extra_parts (field accesses after PASSED)
                for field in extra_parts {
                    if field.as_ref() == ITEM_KEY_FIELD {
                        panic!(
                            "[DD_EVAL] '{}' is internal; Boon code must not access it",
                            ITEM_KEY_FIELD
                        );
                    }
                    // Handle Placeholder specially - create PlaceholderField for field access
                    current = match &current {
                        Value::Placeholder => {
                            Value::PlaceholderField(Arc::new(vec![Arc::from(field.as_str())]))
                        }
                        Value::PlaceholderField(path) => {
                            let mut new_path = (**path).clone();
                            new_path.push(Arc::from(field.as_str()));
                            Value::PlaceholderField(Arc::new(new_path))
                        }
                        // Handle LinkRef.event - create synthetic event object with all event types
                        Value::LinkRef(link_id) if field.as_str() == "event" => {
                            Self::build_link_event_object(link_id)
                        }
                        // Handle Element.event - navigate through nested element.event path
                        Value::Tagged { tag, fields, .. }
                            if tag.as_ref() == "Element" && field.as_str() == "event" =>
                        {
                            fields
                                .get("element")
                                .and_then(|e| e.get("event"))
                                .cloned()
                                .unwrap_or(Value::Unit)
                        }
                        // Forward references are Unit during pass 1; propagate Unit.
                        Value::Unit => Value::Unit,
                        _ => current.get(field.as_str()).cloned().unwrap_or_else(|| {
                            panic!("[DD_EVAL] Bug: missing field '{}' on {:?}", field, current);
                        }),
                    };
                    if matches!(current, Value::List(_)) {
                        list_source_name = Some(field.to_string());
                    }
                }

                // If the result is a Collection, check if there's a reactive HOLD for this field.
                // The interpreter creates HOLDs for reactive list operations (List/append, List/clear).
                // If such a HOLD exists, return CellRef so List/map creates MappedListRef.
                if matches!(current, Value::List(_) | Value::Unit) {
                    if let Some(ref name) = list_source_name {
                        // Check config for a reactive list cell (no IO reads in evaluator)
                        let has_cell = self.dataflow_config.cell_initializations.contains_key(name)
                            || self.dataflow_config.cells.iter().any(|cell| cell.id.name() == name.as_str());
                        if has_cell {
                            dd_log!("[DD_EVAL] PASSED.{} is reactive list - returning CellRef", name);
                            return Value::CellRef(CellId::new(name.as_str()));
                        }
                    }
                }

                current
            }
        }
    }

    /// Replace any LinkRef in a value with the target LinkRef.
    ///
    /// This is used by `|> LINK { alias }` to replace the internally-generated
    /// LinkRef (from the element's `LINK` expression) with the stored LinkRef.
    /// Recursively traverses Objects, Lists, and Tagged values.
    /// Walk a Value tree, replacing every LinkRef with the result of `replacer`.
    /// Shared implementation for replace_link_ref_in_value and replace_link_ref_with_placeholder.
    fn transform_link_refs(&self, value: &Value, replacer: &dyn Fn() -> Value) -> Value {
        use std::collections::BTreeMap;

        match value {
            Value::LinkRef(_) => replacer(),

            Value::Object(fields) => {
                let new_fields: BTreeMap<Arc<str>, Value> = fields
                    .iter()
                    .map(|(k, v)| (k.clone(), self.transform_link_refs(v, replacer)))
                    .collect();
                Value::Object(Arc::new(new_fields))
            }

            Value::Tagged { tag, fields } => {
                let new_fields: BTreeMap<Arc<str>, Value> = fields
                    .iter()
                    .map(|(k, v)| (k.clone(), self.transform_link_refs(v, replacer)))
                    .collect();
                Value::Tagged {
                    tag: tag.clone(),
                    fields: Arc::new(new_fields),
                }
            }

            Value::WhileConfig(config) => {
                let arms: Vec<WhileArm> = config
                    .arms
                    .iter()
                    .map(|arm| WhileArm {
                        pattern: self.transform_link_refs(&arm.pattern, replacer),
                        body: self.transform_link_refs(&arm.body, replacer),
                    })
                    .collect();
                let default = self.transform_link_refs(&config.default, replacer);
                Value::WhileConfig(Arc::new(WhileConfig {
                    cell_id: config.cell_id.clone(),
                    arms: Arc::new(arms),
                    default: Box::new(default),
                }))
            }
            Value::PlaceholderWhile(config) => {
                let arms: Vec<WhileArm> = config
                    .arms
                    .iter()
                    .map(|arm| WhileArm {
                        pattern: self.transform_link_refs(&arm.pattern, replacer),
                        body: self.transform_link_refs(&arm.body, replacer),
                    })
                    .collect();
                let default = self.transform_link_refs(&config.default, replacer);
                Value::PlaceholderWhile(Arc::new(PlaceholderWhileConfig {
                    field_path: config.field_path.clone(),
                    arms: Arc::new(arms),
                    default: Box::new(default),
                }))
            }

            _ => value.clone(),
        }
    }

    fn replace_link_ref_in_value(&self, value: &Value, target_id: &LinkId) -> Value {
        self.transform_link_refs(value, &|| Value::LinkRef(target_id.clone()))
    }

    /// Replace any LinkRef in a value with a placeholder field Tagged value.
    /// Used by `|> LINK { alias }` during template evaluation when the alias
    /// resolves to a placeholder field (deferred field access).
    fn replace_link_ref_with_placeholder(&self, value: &Value, path: &[Arc<str>]) -> Value {
        self.transform_link_refs(value, &|| Value::PlaceholderField(Arc::new(path.to_vec())))
    }

}

impl Default for BoonDdRuntime {
    fn default() -> Self {
        Self::new()
    }
}

/// Check if a Value is a simple DD scalar (suitable for ScalarWhen output).
/// Scalars are values that can be stored in a DD cell and displayed as text.
fn is_dd_scalar(value: &Value) -> bool {
    matches!(
        value,
        Value::Text(_) | Value::Number(_) | Value::Bool(_) | Value::Unit
    )
}

/// Simple function to evaluate expressions and get the document output.
pub fn evaluate_to_document(expressions: &[Spanned<Expression>]) -> Option<Value> {
    let mut runtime = BoonDdRuntime::new();
    runtime.evaluate(expressions);
    runtime.get_document().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{span_at, SourceCode};
    use crate::parser::static_expression::Variable;
    use crate::platform::browser::engine_dd::core::collection_ops::CollectionOp;

    fn spanned_expr(node: Expression) -> Spanned<Expression> {
        Spanned {
            span: span_at(0),
            persistence: None,
            node,
        }
    }

    fn spanned_var(node: Variable) -> Spanned<Variable> {
        Spanned {
            span: span_at(0),
            persistence: None,
            node,
        }
    }

    fn ss(text: &str) -> crate::parser::StrSlice {
        let source = SourceCode::new(text.to_string());
        source.slice(0, text.len())
    }

    fn list_literal_with_key(key: &str) -> Expression {
        let key_var = Variable {
            name: ss("__key"),
            is_referenced: false,
            value: spanned_expr(Expression::Literal(Literal::Text(ss(key)))),
            value_changed: false,
        };
        let obj = Object {
            variables: vec![spanned_var(key_var)],
        };
        Expression::List {
            items: vec![spanned_expr(Expression::Object(obj))],
        }
    }

    #[test]
    fn test_runtime_creation() {
        let runtime = BoonDdRuntime::new();
        assert!(runtime.get_document().is_none());
    }

    #[test]
    fn test_dd_value_operations() {
        let val = Value::int(42);
        assert_eq!(val.to_display_string(), "42");

        let text = Value::text("hello");
        assert_eq!(text.to_display_string(), "hello");
    }

    #[test]
    fn test_latest_concat_list_literals_registers_dd_op() {
        let mut runtime = BoonDdRuntime::new();
        let inputs = vec![
            spanned_expr(list_literal_with_key("a")),
            spanned_expr(list_literal_with_key("b")),
        ];

        let result = runtime.eval_latest(&inputs);
        let handle = match result {
            Value::List(handle) => handle,
            other => panic!("Expected Collection from LATEST concat, found {:?}", other),
        };

        assert_eq!(runtime.dataflow_config.initial_collections.len(), 2);
        assert_eq!(runtime.dataflow_config.collection_ops.len(), 1);

        let op = &runtime.dataflow_config.collection_ops[0];
        assert_eq!(op.output_id, handle.id);
        assert_eq!(handle.cell_id.as_deref(), None);

        match &op.op {
            CollectionOp::Concat { .. } => {}
            other => panic!("Expected Concat op, found {:?}", other),
        }
    }

    #[test]
    fn test_latest_single_list_literal_returns_unbound_collection() {
        let mut runtime = BoonDdRuntime::new();
        let inputs = vec![spanned_expr(list_literal_with_key("a"))];

        let result = runtime.eval_latest(&inputs);
        let handle = match result {
            Value::List(handle) => handle,
            other => panic!("Expected Collection from LATEST, found {:?}", other),
        };
        assert!(handle.cell_id.is_none());
        assert!(runtime.dataflow_config.collection_ops.is_empty());
        assert!(runtime.dataflow_config.initial_collections.contains_key(&handle.id));
    }

    #[test]
    fn test_bind_list_initial_binds_unbound_collection_to_cell_id() {
        let mut runtime = BoonDdRuntime::new();
        let value = runtime.eval_expression(&list_literal_with_key("a"));
        let bound = runtime.bind_list_initial_to_cell("todos", value);
        let handle = match bound {
            Value::List(handle) => handle,
            other => panic!("Expected Collection, found {:?}", other),
        };
        assert_eq!(handle.cell_id.as_deref(), Some("todos"));
        let source_cell = runtime
            .dataflow_config
            .collection_sources
            .get(&handle.id)
            .unwrap_or_else(|| panic!("Missing collection source for {:?}", handle.id));
        assert_eq!(source_cell, "todos");
    }

    #[test]
    #[should_panic(expected = "cannot mix collections with scalar values")]
    fn test_latest_mix_list_and_scalar_panics() {
        let mut runtime = BoonDdRuntime::new();
        let inputs = vec![
            spanned_expr(list_literal_with_key("a")),
            spanned_expr(Expression::Literal(Literal::Number(1.0))),
        ];
        let _ = runtime.eval_latest(&inputs);
    }
}
