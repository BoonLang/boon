//! DD-based evaluator for Boon.
//!
//! This module evaluates Boon AST using simple value types.
//! This is phase 1: static evaluation without reactive features.
//! Phase 2 will add DD-based reactive evaluation.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use super::super::core::value::Value;
use super::super::io::init_cell;
// Phase 11a: add_router_mapping was removed - routing goes through DD dataflow now
use super::super::core::{DataflowConfig, CellConfig, CellId, LinkId, EventFilter, StateTransform, BoolTag, ElementTag, EditingBinding, ToggleBinding, GlobalToggleBinding};

// Global counter for generating unique HOLD IDs across all runtime instances
static GLOBAL_CELL_COUNTER: AtomicU32 = AtomicU32::new(0);

// Global counter for generating unique LINK IDs across all runtime instances
static GLOBAL_LINK_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Reset the global counters (call at start of each evaluation)
pub fn reset_cell_counter() {
    GLOBAL_CELL_COUNTER.store(0, Ordering::SeqCst);
    GLOBAL_LINK_COUNTER.store(0, Ordering::SeqCst);
}
use crate::parser::static_expression::{
    Alias, Arm, ArithmeticOperator, Comparator, Expression, Literal, Object, Pattern, Spanned, TextPart,
};

/// A stored function definition.
#[derive(Clone)]
struct FunctionDef {
    parameters: Vec<String>,
    body: Box<Spanned<Expression>>,
}

/// The DD-based Boon runtime.
pub struct BoonDdRuntime {
    /// Variable values
    variables: HashMap<String, Value>,
    /// Function definitions
    functions: HashMap<String, FunctionDef>,
    /// PASSED context for function calls
    passed_context: Option<Value>,
    /// Counter for generating unique LINK IDs
    link_counter: u32,
    /// Counter for generating unique HOLD IDs
    cell_counter: u32,
    /// Current context path for LINK naming (e.g., "increment_button.event.press")
    context_path: Vec<String>,
    /// Last accessed variable name that contained a List (for source_hold tracking)
    last_list_source: Option<String>,
    /// DataflowConfig built during evaluation (Task 4.4: declarative config builder)
    dataflow_config: DataflowConfig,
    /// Phase 4: Mapping from CellId (HOLD name) to CollectionId for lists
    /// When a HOLD contains a list, we register it here so List/retain, List/map
    /// can look up the source CollectionId for DD operations.
    cell_to_collection: HashMap<String, super::super::core::value::CollectionId>,
}

impl BoonDdRuntime {
    /// Create a new DD runtime.
    pub fn new() -> Self {
        Self {
            variables: HashMap::new(),
            functions: HashMap::new(),
            passed_context: None,
            link_counter: 0,
            cell_counter: 0,
            context_path: Vec::new(),
            last_list_source: None,
            dataflow_config: DataflowConfig::new(),
            cell_to_collection: HashMap::new(),
        }
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
        zoon::println!("[DD_EVAL] Adding CellConfig: id={}, transform={:?}, triggers={:?}",
            config.id.name(), config.transform, config.triggered_by.iter().map(|l| l.name()).collect::<Vec<_>>());
        self.dataflow_config.cells.push(config);
    }

    // ══════════════════════════════════════════════════════════════════════════════
    // Phase 4: Collection operation helpers
    //
    // These methods replace the surgically removed symbolic reference types
    // (FilteredListRef, MappedListRef, ComputedRef, etc.) with DD-native patterns.
    // ══════════════════════════════════════════════════════════════════════════════

    /// Register a list HOLD and get its CollectionId.
    /// Called when evaluating a HOLD that contains a list.
    fn register_list_hold(&mut self, cell_id: &str, items: Vec<Value>) -> super::super::core::value::CollectionId {

        let collection_id = self.dataflow_config.add_initial_collection(items);
        self.cell_to_collection.insert(cell_id.to_string(), collection_id.clone());
        zoon::println!("[DD_EVAL] Registered list HOLD '{}' as CollectionId({:?})", cell_id, collection_id);
        collection_id
    }

    /// Get the CollectionId for a HOLD cell, if it contains a list.
    fn get_collection_id(&self, cell_id: &str) -> Option<super::super::core::value::CollectionId> {
        self.cell_to_collection.get(cell_id).cloned()
    }

    /// Create a filtered collection (replaces FilteredListRef).
    ///
    /// OLD: Value::FilteredListRef { source_hold, filter_field, filter_value }
    /// NEW: Register filter op in DataflowConfig, return Value::Collection
    fn create_filtered_collection(
        &mut self,
        source_cell_id: &str,
        filter_field: std::sync::Arc<str>,
        filter_value: Value,
    ) -> Value {
        use super::super::core::value::CollectionHandle;

        if let Some(source_id) = self.get_collection_id(source_cell_id) {
            let output_id = self.dataflow_config.add_filter(
                source_id,
                Some((filter_field, filter_value)),
                None,
            );
            zoon::println!("[DD_EVAL] Created filtered collection from '{}' -> CollectionId({:?})",
                source_cell_id, output_id);
            Value::Collection(CollectionHandle::new_with_id(output_id))
        } else {
            // Fallback: source not registered as collection, return CellRef
            // This maintains backward compatibility during migration
            zoon::println!("[DD_EVAL] WARNING: source '{}' not registered as collection, returning CellRef", source_cell_id);
            Value::CellRef(CellId::new(source_cell_id))
        }
    }

    /// Create a mapped collection (replaces MappedListRef).
    ///
    /// OLD: Value::MappedListRef { source_hold, element_template }
    /// NEW: Register map op in DataflowConfig, return Value::Collection
    fn create_mapped_collection(
        &mut self,
        source_cell_id: &str,
        element_template: Value,
    ) -> Value {
        use super::super::core::value::CollectionHandle;

        if let Some(source_id) = self.get_collection_id(source_cell_id) {
            let output_id = self.dataflow_config.add_map(source_id, element_template);
            zoon::println!("[DD_EVAL] Created mapped collection from '{}' -> CollectionId({:?})",
                source_cell_id, output_id);
            Value::Collection(CollectionHandle::new_with_id(output_id))
        } else {
            zoon::println!("[DD_EVAL] WARNING: source '{}' not registered as collection, returning CellRef", source_cell_id);
            Value::CellRef(CellId::new(source_cell_id))
        }
    }

    /// Create a list count (replaces ComputedRef::ListCount).
    ///
    /// OLD: Value::ComputedRef { computation: ListCount, source_hold }
    /// NEW: Register count op in DataflowConfig, return Value::Collection
    fn create_list_count(&mut self, source_cell_id: &str) -> Value {
        use super::super::core::value::CollectionHandle;

        if let Some(source_id) = self.get_collection_id(source_cell_id) {
            let output_id = self.dataflow_config.add_count(source_id);
            zoon::println!("[DD_EVAL] Created list count from '{}' -> CollectionId({:?})",
                source_cell_id, output_id);
            Value::Collection(CollectionHandle::new_with_id(output_id))
        } else {
            zoon::println!("[DD_EVAL] WARNING: source '{}' not registered as collection, returning CellRef", source_cell_id);
            Value::CellRef(CellId::new(source_cell_id))
        }
    }

    /// Create a count-where (replaces ComputedRef::ListCountWhere).
    ///
    /// OLD: Value::ComputedRef { computation: ListCountWhere { filter_field, filter_value }, source_hold }
    /// NEW: Register count-where op in DataflowConfig, return Value::Collection
    fn create_list_count_where(
        &mut self,
        source_cell_id: &str,
        filter_field: std::sync::Arc<str>,
        filter_value: Value,
    ) -> Value {
        use super::super::core::value::CollectionHandle;

        if let Some(source_id) = self.get_collection_id(source_cell_id) {
            let output_id = self.dataflow_config.add_count_where(source_id, filter_field, filter_value);
            zoon::println!("[DD_EVAL] Created list count-where from '{}' -> CollectionId({:?})",
                source_cell_id, output_id);
            Value::Collection(CollectionHandle::new_with_id(output_id))
        } else {
            zoon::println!("[DD_EVAL] WARNING: source '{}' not registered as collection, returning CellRef", source_cell_id);
            Value::CellRef(CellId::new(source_cell_id))
        }
    }

    /// Chain a map operation on an existing collection (for filter+map chains).
    ///
    /// OLD: FilteredListRef |> List/map() -> FilteredMappedListRef
    /// NEW: Collection(filtered_id) |> List/map() -> Collection(mapped_id)
    fn chain_map_on_collection(
        &mut self,
        source_id: super::super::core::value::CollectionId,
        element_template: Value,
    ) -> Value {
        use super::super::core::value::CollectionHandle;

        let output_id = self.dataflow_config.add_map(source_id.clone(), element_template);
        zoon::println!("[DD_EVAL] Chained map on CollectionId({:?}) -> CollectionId({:?})",
            source_id, output_id);
        Value::Collection(CollectionHandle::new_with_id(output_id))
    }

    /// Chain a filter operation on an existing collection (for chained filters).
    fn chain_filter_on_collection(
        &mut self,
        source_id: super::super::core::value::CollectionId,
        filter_field: Option<(std::sync::Arc<str>, Value)>,
        predicate_template: Option<Value>,
    ) -> Value {
        use super::super::core::value::CollectionHandle;

        let output_id = self.dataflow_config.add_filter(source_id.clone(), filter_field, predicate_template);
        zoon::println!("[DD_EVAL] Chained filter on CollectionId({:?}) -> CollectionId({:?})",
            source_id, output_id);
        Value::Collection(CollectionHandle::new_with_id(output_id))
    }

    /// Create a list is_empty check (replaces ComputedRef::ListIsEmpty).
    fn create_list_is_empty(&mut self, source_cell_id: &str) -> Value {
        use super::super::core::value::CollectionHandle;

        if let Some(source_id) = self.get_collection_id(source_cell_id) {
            let output_id = self.dataflow_config.add_is_empty(source_id);
            zoon::println!("[DD_EVAL] Created list is_empty from '{}' -> CollectionId({:?})",
                source_cell_id, output_id);
            Value::Collection(CollectionHandle::new_with_id(output_id))
        } else {
            zoon::println!("[DD_EVAL] WARNING: source '{}' not registered as collection, returning CellRef", source_cell_id);
            Value::CellRef(CellId::new(source_cell_id))
        }
    }

    /// Chain a count operation on an existing collection.
    fn chain_count_on_collection(
        &mut self,
        source_id: super::super::core::value::CollectionId,
    ) -> Value {
        use super::super::core::value::CollectionHandle;

        let output_id = self.dataflow_config.add_count(source_id.clone());
        zoon::println!("[DD_EVAL] Chained count on CollectionId({:?}) -> CollectionId({:?})",
            source_id, output_id);
        Value::Collection(CollectionHandle::new_with_id(output_id))
    }

    /// Chain an is_empty operation on an existing collection.
    fn chain_is_empty_on_collection(
        &mut self,
        source_id: super::super::core::value::CollectionId,
    ) -> Value {
        use super::super::core::value::CollectionHandle;

        let output_id = self.dataflow_config.add_is_empty(source_id.clone());
        zoon::println!("[DD_EVAL] Chained is_empty on CollectionId({:?}) -> CollectionId({:?})",
            source_id, output_id);
        Value::Collection(CollectionHandle::new_with_id(output_id))
    }

    // ══════════════════════════════════════════════════════════════════════════════
    // Phase 4: Arithmetic/Comparison Operation Helpers
    // Replaces: ComputedRef::Subtract, ComputedRef::GreaterThanZero, ComputedRef::Equal
    // ══════════════════════════════════════════════════════════════════════════════

    /// Create a subtract operation (left - right).
    /// Replaces: ComputedRef::Subtract
    /// Used for: active_list_count = list_count - completed_list_count
    fn create_subtract(
        &mut self,
        left: super::super::core::value::CollectionId,
        right: super::super::core::value::CollectionId,
    ) -> Value {
        use super::super::core::value::CollectionHandle;

        let output_id = self.dataflow_config.add_subtract(left.clone(), right.clone());
        zoon::println!("[DD_EVAL] Created subtract: CollectionId({:?}) - CollectionId({:?}) -> CollectionId({:?})",
            left, right, output_id);
        Value::Collection(CollectionHandle::new_with_id(output_id))
    }

    /// Create a greater-than-zero check.
    /// Replaces: ComputedRef::GreaterThanZero
    /// Used for: show_clear_completed = completed_list_count > 0
    fn create_greater_than_zero(
        &mut self,
        source_id: super::super::core::value::CollectionId,
    ) -> Value {
        use super::super::core::value::CollectionHandle;

        let output_id = self.dataflow_config.add_greater_than_zero(source_id.clone());
        zoon::println!("[DD_EVAL] Created greater_than_zero: CollectionId({:?}) > 0 -> CollectionId({:?})",
            source_id, output_id);
        Value::Collection(CollectionHandle::new_with_id(output_id))
    }

    /// Create an equality comparison.
    /// Replaces: ComputedRef::Equal
    /// Used for: all_completed = completed_list_count == list_count
    fn create_equal(
        &mut self,
        left: super::super::core::value::CollectionId,
        right: super::super::core::value::CollectionId,
    ) -> Value {
        use super::super::core::value::CollectionHandle;

        let output_id = self.dataflow_config.add_equal(left.clone(), right.clone());
        zoon::println!("[DD_EVAL] Created equal: CollectionId({:?}) == CollectionId({:?}) -> CollectionId({:?})",
            left, right, output_id);
        Value::Collection(CollectionHandle::new_with_id(output_id))
    }

    // ══════════════════════════════════════════════════════════════════════════════
    // End Phase 4 helpers
    // ══════════════════════════════════════════════════════════════════════════════

    /// Determine the StateTransform from HOLD body pattern.
    ///
    /// Patterns detected:
    /// - `state |> Bool/not()` → BoolToggle
    /// - `state + 1` → Increment
    /// - `True` or `False` tag → SetTrue/SetFalse
    /// - Default → Identity (no transform, just propagate event)
    fn determine_transform(&self, body: &Expression, state_name: &str, initial: &Value) -> StateTransform {
        // Look for patterns in THEN body
        match body {
            Expression::Pipe { from, to } => {
                // Pattern: event |> THEN { transform_body }
                if let Expression::Then { body: then_body } = &to.node {
                    return self.determine_transform_from_then_body(&then_body.node, state_name, initial);
                }
                // Pattern: LATEST { ... } |> something
                if let Expression::Latest { inputs, .. } = &from.node {
                    // Check first input that has THEN
                    for input in inputs {
                        if let Some(transform) = self.determine_transform_from_input(&input.node, state_name, initial) {
                            return transform;
                        }
                    }
                }
                StateTransform::Identity
            }
            Expression::Latest { inputs, .. } => {
                // Check inputs for transform patterns
                for input in inputs {
                    if let Some(transform) = self.determine_transform_from_input(&input.node, state_name, initial) {
                        return transform;
                    }
                }
                StateTransform::Identity
            }
            _ => StateTransform::Identity,
        }
    }

    /// Helper to determine transform from a single LATEST input.
    fn determine_transform_from_input(&self, input: &Expression, state_name: &str, initial: &Value) -> Option<StateTransform> {
        match input {
            Expression::Pipe { from: _, to } => {
                if let Expression::Then { body: then_body } = &to.node {
                    let transform = self.determine_transform_from_then_body(&then_body.node, state_name, initial);
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
    fn determine_transform_from_then_body(&self, body: &Expression, state_name: &str, initial: &Value) -> StateTransform {
        match body {
            // Pattern: state |> Bool/not() → BoolToggle
            Expression::Pipe { from, to } => {
                if let Expression::Variable(var) = &from.node {
                    if var.name.as_ref() == state_name {
                        if let Expression::FunctionCall { path, .. } = &to.node {
                            let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                            if path_strs == ["Bool", "not"] {
                                return StateTransform::BoolToggle;
                            }
                        }
                    }
                }
                StateTransform::Identity
            }
            // Pattern: state + 1 → Increment
            Expression::ArithmeticOperator(ArithmeticOperator::Add { operand_a, .. }) => {
                // Check if left operand is state
                if let Expression::Variable(var) = &operand_a.node {
                    if var.name.as_ref() == state_name {
                        return StateTransform::Increment;
                    }
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
            // Check if initial is boolean - might be an implicit toggle
            _ if matches!(initial, Value::Bool(_)) => {
                StateTransform::BoolToggle
            }
            _ => StateTransform::Identity,
        }
    }

    /// Generate a unique HOLD ID using global counter.
    fn generate_cell_id(&mut self) -> String {
        let id = GLOBAL_CELL_COUNTER.fetch_add(1, Ordering::SeqCst);
        format!("hold_{}", id)
    }

    /// Generate a unique LINK ID using global counter.
    fn generate_link_id(&mut self) -> String {
        let id = GLOBAL_LINK_COUNTER.fetch_add(1, Ordering::SeqCst);
        if self.context_path.is_empty() {
            format!("link_{}", id + 1)
        } else {
            self.context_path.join(".")
        }
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

    /// Get all variables.
    pub fn get_all_variables(&self) -> &HashMap<String, Value> {
        &self.variables
    }

    /// Get all defined function names.
    /// Used by interpreter to detect available template functions.
    pub fn get_function_names(&self) -> Vec<&String> {
        self.functions.keys().collect()
    }

    /// Get the parameter names for a function.
    /// Used by interpreter to detect correct parameter names for element templates.
    pub fn get_function_parameters(&self, name: &str) -> Option<&[String]> {
        self.functions.get(name).map(|def| def.parameters.as_slice())
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
        let mut func_runtime = BoonDdRuntime {
            variables: self.variables.clone(),
            functions: self.functions.clone(),
            passed_context: self.passed_context.clone(),
            link_counter: 0,
            cell_counter: 0,
            context_path: Vec::new(),
            last_list_source: None,
            dataflow_config: DataflowConfig::new(),
            cell_to_collection: HashMap::new(),
        };

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

        // First pass: evaluate all variables (forward refs will be Unit)
        // Skip pre-injected variables
        for expr in expressions {
            if let Expression::Variable(var) = &expr.node {
                let name = var.name.as_str().to_string();
                if !injected_vars.contains(&name) {
                    let value = self.eval_expression(&var.value.node);
                    self.variables.insert(name, value);
                }
            }
        }

        // Second pass: re-evaluate to resolve forward references
        // Now all variable names are defined, so references should resolve
        // Skip pre-injected variables
        for expr in expressions {
            if let Expression::Variable(var) = &expr.node {
                let name = var.name.as_str().to_string();
                if injected_vars.contains(&name) {
                    continue; // Keep pre-injected value
                }
                let value = self.eval_expression(&var.value.node);
                #[cfg(debug_assertions)]
                zoon::println!("[DD_EVAL] {} = {:?}", name, value);
                self.variables.insert(name, value);
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
                let values: Vec<Value> = items
                    .iter()
                    .map(|spanned| self.eval_expression(&spanned.node))
                    .collect();
                Value::list(values)
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

                        zoon::println!("[DD_EVAL] Timer+sum pattern detected: {} @ {}ms", cell_id, interval_ms);

                        // Task 6.3: Build CellConfig during evaluation (eliminates interpreter fallback)
                        self.add_cell_config(CellConfig {
                            id: CellId::new(cell_id),
                            initial: Value::int(0), // Timer counters start at 0
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
                self.eval_pipe(&from_val, &to.node)
            }

            // Block: BLOCK { vars, result }
            Expression::Block { variables, output } => {
                let mut block_runtime = BoonDdRuntime {
                    variables: self.variables.clone(),
                    functions: self.functions.clone(),
                    passed_context: self.passed_context.clone(),
                    link_counter: 0,
                    cell_counter: 0,
                    context_path: Vec::new(),
                    last_list_source: None,
                    dataflow_config: DataflowConfig::new(),
                    cell_to_collection: HashMap::new(),
                };
                for var in variables {
                    let name = var.node.name.as_str().to_string();
                    let value = block_runtime.eval_expression(&var.node.value.node);
                    block_runtime.variables.insert(name, value);
                }
                block_runtime.eval_expression(&output.node)
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
            Literal::Tag(s) => Value::Tagged {
                tag: Arc::from(s.as_str()),
                fields: Arc::new(BTreeMap::new()),
            },
        }
    }

    /// Evaluate an object.
    ///
    /// Uses a scoped runtime so variables defined earlier in the object
    /// can be referenced by variables defined later (forward references
    /// within the same object).
    fn eval_object(&mut self, obj: &Object) -> Value {
        // Create a scoped runtime with access to current variables
        let mut scoped_runtime = BoonDdRuntime {
            variables: self.variables.clone(),
            functions: self.functions.clone(),
            passed_context: self.passed_context.clone(),
            link_counter: 0,
            cell_counter: 0,
            context_path: Vec::new(),
                last_list_source: None,
            dataflow_config: DataflowConfig::new(),
            cell_to_collection: HashMap::new(),
            };

        let mut map = BTreeMap::new();
        for var in &obj.variables {
            let name_str = var.node.name.as_str();
            let name = Arc::from(name_str);

            // Check if this field contains reactive list operations (List/append, List/clear)
            // If so, evaluate the initial list, store in HOLD, and return CellRef
            let value = if self.contains_reactive_list_ops(&var.node.value.node) {
                zoon::println!("[DD_EVAL] Field '{}' has reactive list ops - creating CellRef", name_str);

                // Extract and evaluate the initial list expression (before List/append, List/clear, List/remove)
                if let Some(initial_expr) = self.extract_initial_list_expr(&var.node.value.node) {
                    let initial_value = scoped_runtime.eval_expression(initial_expr);
                    zoon::println!("[DD_EVAL] Field '{}' initial list value: {:?}", name_str, initial_value);

                    // Store the initial value in the HOLD
                    init_cell(name_str, initial_value);
                }

                // Parse List/remove event paths from the pipe chain (since eval_pipe is bypassed)
                scoped_runtime.parse_list_remove_bindings(&var.node.value.node);

                Value::CellRef(CellId::new(name_str))
            } else {
                scoped_runtime.eval_expression(&var.node.value.node)
            };

            // Add to both the result map and the scoped runtime
            // so later variables can reference earlier ones
            map.insert(name, value.clone());
            scoped_runtime.variables.insert(name_str.to_string(), value);
        }
        Value::Object(Arc::new(map))
    }

    /// Check if an expression contains reactive list operations (List/append, List/clear).
    /// Used to detect fields that should become reactive HOLDs.
    fn contains_reactive_list_ops(&self, expr: &Expression) -> bool {
        match expr {
            Expression::Pipe { from, to } => {
                // Check if piping to List/append or List/clear
                if let Expression::FunctionCall { path, .. } = &to.node {
                    let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                    if path_strs == ["List", "append"] || path_strs == ["List", "clear"] {
                        return true;
                    }
                }
                // Recursively check both sides of pipe
                self.contains_reactive_list_ops(&from.node) || self.contains_reactive_list_ops(&to.node)
            }
            Expression::FunctionCall { path, .. } => {
                let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                path_strs == ["List", "append"] || path_strs == ["List", "clear"]
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
    fn parse_list_remove_bindings(&self, expr: &Expression) {
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
                            // Try to extract per-item removal path (item.X.Y.event.press → ["X", "Y"])
                            if let Some(path) = self.extract_linkref_path_from_event(binding, &event_expr.node) {
                                zoon::println!("[DD_EVAL] List/remove (reactive field) parsed on: binding={}, path={:?}", binding, path);
                                self.dataflow_config.set_remove_event_path(path);
                            } else {
                                // Try to extract bulk removal path (elements.X.event.press → global event)
                                if let Some(global_path) = self.extract_global_event_path(&event_expr.node) {
                                    zoon::println!("[DD_EVAL] List/remove (reactive field) parsed BULK on: path={:?}", global_path);
                                    self.dataflow_config.set_bulk_remove_event_path(global_path);
                                }
                            }
                        }
                    }
                }
                // Recursively check both sides of pipe
                self.parse_list_remove_bindings(&from.node);
                self.parse_list_remove_bindings(&to.node);
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
                        if let Some(path) = self.extract_linkref_path_from_event(binding, &event_expr.node) {
                            zoon::println!("[DD_EVAL] List/remove (direct) parsed on: binding={}, path={:?}", binding, path);
                            self.dataflow_config.set_remove_event_path(path);
                        } else if let Some(global_path) = self.extract_global_event_path(&event_expr.node) {
                            zoon::println!("[DD_EVAL] List/remove (direct) parsed BULK on: path={:?}", global_path);
                            self.dataflow_config.set_bulk_remove_event_path(global_path);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Evaluate a text literal with interpolation.
    /// If any interpolated value is reactive (ComputedRef, WhileRef, CellRef),
    /// create a ReactiveText that the bridge evaluates at render time.
    fn eval_text_literal(&mut self, parts: &[TextPart]) -> Value {
        // First pass: collect values and check for reactive parts
        let mut collected_parts: Vec<Value> = Vec::new();
        let mut has_reactive = false;

        for part in parts {
            match part {
                TextPart::Text(s) => {
                    collected_parts.push(Value::text(s.as_str()));
                }
                TextPart::Interpolation { var, .. } => {
                    if let Some(value) = self.variables.get(var.as_str()) {
                        // Check if the value is reactive or a placeholder
                        // Phase 4: Removed ComputedRef, WhileRef - use CellRef and Collection for reactivity
                        let is_reactive = matches!(
                            value,
                            Value::CellRef(_) |
                            Value::Collection(_) |
                            Value::Placeholder
                        );
                        if is_reactive {
                            has_reactive = true;
                        }
                        collected_parts.push(value.clone());
                    }
                }
            }
        }

        // Phase 4: For now, always evaluate text at evaluation time
        // TODO: Add DD text concatenation operator for true reactive text
        let result: String = collected_parts.iter()
            .map(|v| v.to_display_string())
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
            return Value::Unit;
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
                args.get("root").cloned().unwrap_or(Value::Unit)
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
            _ => Value::Unit,
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
            .unwrap_or(Value::Unit);

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
        if let Some(func_def) = self.functions.get(name) {
            #[cfg(debug_assertions)]
            if name == "filter_button" {
                zoon::println!("[DD_EVAL] filter_button called with args: {:?}", args);
            }

            // Check for PASS: argument to set passed_context
            let passed_context = args.get("PASS").cloned().or_else(|| self.passed_context.clone());

            // Create a new runtime with the function arguments as variables
            let mut func_runtime = BoonDdRuntime {
                variables: self.variables.clone(),
                functions: self.functions.clone(),
                passed_context,
                link_counter: 0,
                cell_counter: 0,
                context_path: Vec::new(),
                last_list_source: None,
            dataflow_config: DataflowConfig::new(),
            cell_to_collection: HashMap::new(),
            };

            // Bind arguments to parameters
            for (param, arg_name) in func_def.parameters.iter().zip(args.keys()) {
                if let Some(value) = args.get(*arg_name) {
                    func_runtime.variables.insert(param.clone(), value.clone());
                }
            }

            // Also bind by parameter name directly (for named arguments)
            for param in &func_def.parameters {
                if let Some(value) = args.get(param.as_str()) {
                    func_runtime.variables.insert(param.clone(), value.clone());
                }
            }

            #[cfg(debug_assertions)]
            if name == "filter_button" {
                zoon::println!("[DD_EVAL] filter_button 'filter' var = {:?}", func_runtime.variables.get("filter"));
                zoon::println!("[DD_EVAL] filter_button passed_context.store.selected_filter = {:?}",
                    func_runtime.passed_context.as_ref()
                        .and_then(|p| p.get("store"))
                        .and_then(|s| s.get("selected_filter")));
            }

            // Evaluate the function body
            func_runtime.eval_expression(&func_def.body.node)
        } else {
            Value::Unit
        }
    }

    /// Evaluate a user-defined function with a piped value.
    /// The piped value becomes the first parameter of the function.
    fn eval_user_function_with_piped(
        &self,
        name: &str,
        piped: &Value,
        args: &HashMap<&str, Value>,
    ) -> Value {
        if let Some(func_def) = self.functions.get(name) {
            // Check for PASS: argument to set passed_context
            let passed_context = args.get("PASS").cloned().or_else(|| self.passed_context.clone());

            // Create a new runtime with the function arguments as variables
            let mut func_runtime = BoonDdRuntime {
                variables: self.variables.clone(),
                functions: self.functions.clone(),
                passed_context,
                link_counter: 0,
                cell_counter: 0,
                context_path: Vec::new(),
                last_list_source: None,
            dataflow_config: DataflowConfig::new(),
            cell_to_collection: HashMap::new(),
            };

            // First parameter gets the piped value
            if let Some(first_param) = func_def.parameters.first() {
                func_runtime.variables.insert(first_param.clone(), piped.clone());
            }

            // Bind remaining named arguments
            for param in &func_def.parameters {
                if let Some(value) = args.get(param.as_str()) {
                    func_runtime.variables.insert(param.clone(), value.clone());
                }
            }

            // Evaluate the function body
            func_runtime.eval_expression(&func_def.body.node)
        } else {
            Value::Unit
        }
    }

    /// Evaluate an Element function.
    fn eval_element_function(&mut self, name: &str, args: &HashMap<&str, Value>) -> Value {
        zoon::println!("[DD_EVAL] Element/{}() called with args: {:?}", name, args.keys().collect::<Vec<_>>());
        let mut fields: Vec<(&str, Value)> = vec![("_element_type", Value::text(name))];
        for (k, v) in args {
            fields.push((k, v.clone()));
        }

        // NOTE: text_input key_down detection moved to LinkSetter (see eval_pipe)
        // LinkSetter detects with the FINAL link ID after replacement, not the temporary one

        let result = Value::tagged("Element", fields.into_iter());
        zoon::println!("[DD_EVAL] Element/{}() -> Tagged(Element)", name);
        result
    }

    /// Evaluate a List function.
    fn eval_list_function(&mut self, name: &str, args: &HashMap<&str, Value>) -> Value {
        match name {
            "count" => {
                if let Some(Value::List(items)) = args.values().next() {
                    Value::int(items.len() as i64)
                } else {
                    Value::int(0)
                }
            }
            "is_empty" => {
                if let Some(Value::List(items)) = args.values().next() {
                    Value::Bool(items.is_empty())
                } else {
                    Value::Bool(true)
                }
            }
            _ => Value::Unit,
        }
    }

    /// Evaluate a Router function.
    fn eval_router_function(&mut self, name: &str, _args: &HashMap<&str, Value>) -> Value {
        match name {
            // Router/route() - returns a CellRef to the current route
            // The actual route value is stored in CELL_STATES["current_route"]
            // and updated by navigation events via set_filter_from_route()
            "route" => {
                // Initialize the current_route HOLD with the browser's current path
                #[cfg(target_arch = "wasm32")]
                {
                    super::super::io::init_current_route();
                    let path = super::super::io::get_current_route();
                    // Store in CELL_STATES so bridge can render it reactively
                    init_cell("current_route", Value::text(path));
                }
                #[cfg(not(target_arch = "wasm32"))]
                {
                    init_cell("current_route", Value::text("/"));
                }
                // Return CellRef so WHEN can observe route changes reactively
                Value::CellRef(CellId::new("current_route"))
            }
            // Router/go_to(route) - navigation (no-op in static context)
            "go_to" => Value::Unit,
            _ => Value::Unit,
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
                    Value::text("")
                }
            }
            // Text/is_not_empty() -> Bool
            "is_not_empty" => {
                if let Some(Value::Text(s)) = args.values().next() {
                    Value::Bool(!s.is_empty())
                } else {
                    Value::Bool(false)
                }
            }
            // Text/is_empty() -> Bool
            "is_empty" => {
                if let Some(Value::Text(s)) = args.values().next() {
                    Value::Bool(s.is_empty())
                } else {
                    Value::Bool(true)
                }
            }
            // Text/empty() -> ""
            "empty" => Value::text(""),
            _ => Value::Unit,
        }
    }

    /// Evaluate a pipe expression.
    fn eval_pipe(&mut self, from: &Value, to: &Expression) -> Value {
        match to {
            // Pipe to function call
            Expression::FunctionCall { path, arguments } => {
                let full_path: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                let (namespace, name) = if full_path.len() >= 2 {
                    (Some(full_path[0]), full_path[1])
                } else if full_path.len() == 1 {
                    (None, full_path[0])
                } else {
                    return Value::Unit;
                };

                // Build args
                let args: HashMap<&str, Value> = arguments
                    .iter()
                    .filter_map(|arg| {
                        let name = arg.node.name.as_str();
                        let value = arg.node.value.as_ref().map(|v| self.eval_expression(&v.node))?;
                        Some((name, value))
                    })
                    .collect();

                match (namespace, name) {
                    (Some("Document"), "new") => {
                        // from |> Document/new() means from is the root
                        if !args.contains_key("root") {
                            return from.clone();
                        }
                        args.get("root").cloned().unwrap_or(Value::Unit)
                    }
                    (Some("Math"), "sum") => {
                        // LatestRef |> Math/sum() - create a reactive CellRef for accumulation
                        // Phase 4: LatestRef removed - DD handles event merging natively
                        // If input is already a CellRef, it's a reactive accumulator
                        if let Value::CellRef(cell_id) = from {
                            zoon::println!("[DD_EVAL] CellRef |> Math/sum(): {:?}", cell_id);
                            // DD already handles accumulation via HOLD - return as-is
                            return from.clone();
                        }
                        // TimerRef |> Math/sum() - also creates a reactive accumulator
                        if let Value::TimerRef { interval_ms, .. } = from {
                            let cell_id = "timer_counter";
                            // NOTE: Do NOT call init_cell here!
                            // The test expects empty output until the first timer fires.
                            zoon::println!("[DD_EVAL] TimerRef |> Math/sum(): {} @ {}ms", cell_id, interval_ms);
                            return Value::TimerRef {
                                id: Arc::from(cell_id),
                                interval_ms: *interval_ms,
                            };
                        }
                        // Static value - just pass through
                        from.clone()
                    }
                    (Some("Router"), "go_to") => {
                        // Phase 11a: ROUTER_MAPPINGS removed - routing now goes through DD
                        // Router/go_to() is now handled by DD output observer
                        // Simply pass through the input - DD will process link events
                        zoon::println!("[DD_EVAL] Router/go_to(): DD-native routing (Phase 11a)");
                        Value::Unit
                    }
                    (Some("Timer"), "interval") => {
                        // Duration |> Timer/interval() - returns TimerRef
                        // Extract interval from Duration[seconds: n] or Duration[millis: n]
                        let interval_ms = match from {
                            Value::Tagged { tag, fields } if tag.as_ref() == "Duration" => {
                                if let Some(Value::Number(secs)) = fields.get("seconds") {
                                    (secs.0 * 1000.0) as u64
                                } else if let Some(Value::Number(ms)) = fields.get("millis") {
                                    ms.0 as u64
                                } else {
                                    1000 // Default 1 second
                                }
                            }
                            Value::Number(ms) => ms.0 as u64,
                            _ => 1000, // Default 1 second
                        };
                        let timer_id = format!("timer_{}", interval_ms);
                        zoon::println!("[DD_EVAL] Timer/interval: {}ms -> {}", interval_ms, timer_id);
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
                        from.clone()
                    }
                    (Some("List"), "clear") => {
                        // from |> List/clear(on: ...) - pass through for static eval
                        // The clear operation depends on events (e.g., button press)
                        // so we don't clear items during static evaluation.
                        //
                        // Task 6.3: LinkRef detection moved to LinkSetter
                        // The on: argument references a LINK placeholder (e.g., elements.clear_button)
                        // which gets its actual LinkRef when the button is linked via |> LINK { ... }
                        // LinkSetter detects button press events and stores them via set_list_clear_link()
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

                        if let (Some(binding), Some(event_expr)) = (binding_name, on_expr) {
                            // Try to extract per-item removal path (item.X.Y.event.press → ["X", "Y"])
                            if let Some(path) = self.extract_linkref_path_from_event(binding, &event_expr.node) {
                                zoon::println!("[DD_EVAL] List/remove parsed on: binding={}, path={:?}", binding, path);
                                self.dataflow_config.set_remove_event_path(path);
                            } else {
                                // Try to extract bulk removal path (elements.X.event.press → global event)
                                // This handles patterns like: elements.remove_completed_button.event.press |> THEN {...}
                                if let Some(global_path) = self.extract_global_event_path(&event_expr.node) {
                                    zoon::println!("[DD_EVAL] List/remove parsed BULK on: path={:?}", global_path);
                                    self.dataflow_config.set_bulk_remove_event_path(global_path);
                                }
                            }
                        }

                        // Pass through for static eval (removal depends on events)
                        from.clone()
                    }
                    (Some("List"), "retain") => {
                        // from |> List/retain(item, if: ...) - filter items based on predicate
                        // Phase 4: Uses DD-native collection filter instead of FilteredListRef

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

                        // Handle CellRef input - create DD-native filtered collection
                        if let Value::CellRef(cell_id) = from {
                            if let (Some(binding), Some(pred_expr)) = (binding_name, predicate_expr) {
                                // Try to extract field access pattern: `item.field`
                                if let Some((field_name, filter_value)) =
                                    self.extract_field_filter(binding, &pred_expr.node)
                                {
                                    // Phase 4: Use DD-native filter instead of FilteredListRef
                                    return self.create_filtered_collection(
                                        cell_id.name(),
                                        Arc::from(field_name),
                                        filter_value,
                                    );
                                }
                                // Complex predicate: create predicate template with Placeholder
                                // Evaluate the predicate with item = Placeholder to create a template
                                let mut template_runtime = BoonDdRuntime {
                                    variables: self.variables.clone(),
                                    functions: self.functions.clone(),
                                    passed_context: self.passed_context.clone(),
                                    link_counter: 0,
                                    cell_counter: 0,
                                    context_path: Vec::new(),
                                    last_list_source: None,
                                    dataflow_config: DataflowConfig::new(),
                                    cell_to_collection: HashMap::new(),
                                };
                                template_runtime.variables.insert(binding.to_string(), Value::Placeholder);
                                let predicate_template = template_runtime.eval_expression(&pred_expr.node);
                                zoon::println!("[DD_EVAL] Phase 4: Complex predicate filter on '{}', template={:?}",
                                    cell_id.name(), predicate_template);

                                // Phase 4: Register filter with predicate template
                                // For now, return CellRef - complex predicates need worker support
                                // TODO: Implement predicate evaluation in DD worker
                                return Value::CellRef(cell_id.clone());
                            }
                            // Fallback: return CellRef unchanged
                            return from.clone();
                        }

                        // Handle concrete List input - check if filter references CellRef fields
                        if let (Some(binding), Some(pred_expr), Value::List(items)) =
                            (binding_name, predicate_expr, from)
                        {
                            // Try to extract field access pattern: `item.field`
                            if let Some((field_name, filter_value)) =
                                self.extract_field_filter(binding, &pred_expr.node)
                            {
                                // Check if the field contains CellRefs in any item
                                let cell_ids: Vec<CellId> = items
                                    .iter()
                                    .filter_map(|item| {
                                        if let Value::Object(obj) = item {
                                            if let Some(Value::CellRef(cell_id)) = obj.get(field_name.as_str()) {
                                                return Some(cell_id.clone());
                                            }
                                        }
                                        None
                                    })
                                    .collect();

                                // Phase 4: If all items have CellRef fields, use DD-native filter
                                // OLD: Created ReactiveFilteredList for deferred evaluation
                                // NEW: Register filter operation and return Collection
                                if cell_ids.len() == items.len() && !cell_ids.is_empty() {
                                    let source_hold = self.get_list_source();
                                    zoon::println!("[DD_EVAL] Phase 4: Reactive filter on '{}', {} items with CellRef fields",
                                        source_hold, items.len());
                                    // Register the source collection if not already done
                                    if self.get_collection_id(&source_hold).is_none() {
                                        self.register_list_hold(&source_hold, items.to_vec());
                                    }
                                    return self.create_filtered_collection(
                                        &source_hold,
                                        Arc::from(field_name),
                                        filter_value,
                                    );
                                }
                            }

                            // Fallback: filter immediately (for non-reactive cases)
                            let filtered: Vec<Value> = items
                                .iter()
                                .filter(|item| {
                                    // Create scoped runtime with the item bound
                                    let mut scoped = BoonDdRuntime {
                                        variables: self.variables.clone(),
                                        functions: self.functions.clone(),
                                        passed_context: self.passed_context.clone(),
                                        link_counter: 0,
                                        cell_counter: 0,
                                        context_path: Vec::new(),
                                        last_list_source: None,
                                    dataflow_config: DataflowConfig::new(),
                                    cell_to_collection: HashMap::new(),
                                    };
                                    scoped.variables.insert(binding.to_string(), (*item).clone());

                                    // Evaluate the predicate
                                    let result = scoped.eval_expression(&pred_expr.node);
                                    result.is_truthy()
                                })
                                .cloned()
                                .collect();
                            Value::list(filtered)
                        } else {
                            from.clone()
                        }
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

                        match (binding_name, transform_expr, from) {
                            // Phase 4: CellRef |> List/map(item, new: ...) -> DD mapped collection
                            // Registers a map operation in the DD dataflow graph
                            (Some(binding), Some(new_expr), Value::CellRef(cell_id)) => {
                                // Evaluate the transform with Placeholder as the item
                                // This creates a template that can be substituted at render time
                                let mut scoped = BoonDdRuntime {
                                    variables: self.variables.clone(),
                                    functions: self.functions.clone(),
                                    passed_context: self.passed_context.clone(),
                                    link_counter: 0,
                                    cell_counter: 0,
                                    context_path: Vec::new(),
                                    last_list_source: None,
                                    dataflow_config: DataflowConfig::new(),
                                    cell_to_collection: HashMap::new(),
                                };
                                scoped.variables.insert(binding.to_string(), Value::Placeholder);

                                let element_template = scoped.eval_expression(&new_expr.node);
                                zoon::println!("[DD_EVAL] List/map on CellRef: source={}, template={:?}", cell_id, element_template);

                                // Phase 4: Use DD-native mapped collection instead of MappedListRef
                                self.create_mapped_collection(cell_id.name(), element_template)
                            }
                            // Phase 4: Collection |> List/map(item, new: ...) -> chained DD map
                            // This handles filter+map chains: Collection(filtered) |> List/map()
                            (Some(binding), Some(new_expr), Value::Collection(handle)) => {
                                // Evaluate the transform with Placeholder as the item
                                let mut scoped = BoonDdRuntime {
                                    variables: self.variables.clone(),
                                    functions: self.functions.clone(),
                                    passed_context: self.passed_context.clone(),
                                    link_counter: 0,
                                    cell_counter: 0,
                                    context_path: Vec::new(),
                                    last_list_source: None,
                                    dataflow_config: DataflowConfig::new(),
                                    cell_to_collection: HashMap::new(),
                                };
                                scoped.variables.insert(binding.to_string(), Value::Placeholder);

                                let element_template = scoped.eval_expression(&new_expr.node);
                                zoon::println!("[DD_EVAL] List/map on Collection: source={:?}, template={:?}",
                                    handle.id, element_template);

                                // Phase 4: Chain map operation on existing collection (e.g., filtered collection)
                                self.chain_map_on_collection(handle.id.clone(), element_template)
                            }
                            // List |> List/map(item, new: ...) -> concrete list
                            (Some(binding), Some(new_expr), Value::List(items)) => {
                                let mapped: Vec<Value> = items
                                    .iter()
                                    .map(|item| {
                                        // Create scoped runtime with the item bound
                                        let mut scoped = BoonDdRuntime {
                                            variables: self.variables.clone(),
                                            functions: self.functions.clone(),
                                            passed_context: self.passed_context.clone(),
                                            link_counter: 0,
                                            cell_counter: 0,
                                            context_path: Vec::new(),
                                            last_list_source: None,
                                        dataflow_config: DataflowConfig::new(),
                                        cell_to_collection: HashMap::new(),
                                        };
                                        scoped.variables.insert(binding.to_string(), (*item).clone());

                                        // Evaluate the transform expression
                                        scoped.eval_expression(&new_expr.node)
                                    })
                                    .collect();
                                Value::list(mapped)
                            }
                            _ => from.clone(),
                        }
                    }
                    (Some("List"), "count") => {
                        // Phase 4: from |> List/count() - DD-native count operation
                        match from {
                            Value::List(items) => {
                                // Check if this list came from a HOLD variable
                                let source_hold = self.get_list_source();
                                if !source_hold.name().is_empty() {
                                    // Phase 4: Use DD-native count instead of ComputedRef
                                    zoon::println!("[DD_EVAL] List/count() on List from HOLD: source_hold={}", source_hold);
                                    self.create_list_count(source_hold.name())
                                } else {
                                    // Static count for lists not from HOLD
                                    Value::int(items.len() as i64)
                                }
                            }
                            // Phase 4: CellRef |> List/count() -> DD-native count
                            Value::CellRef(cell_id) => {
                                zoon::println!("[DD_EVAL] List/count() on CellRef: {}", cell_id);
                                self.create_list_count(cell_id.name())
                            }
                            // Phase 4: Collection |> List/count() -> chain count on collection
                            // Handles chained operations like filter+count
                            Value::Collection(handle) => {
                                zoon::println!("[DD_EVAL] List/count() on Collection: {:?}", handle.id);
                                self.chain_count_on_collection(handle.id.clone())
                            }
                            _ => Value::int(0),
                        }
                    }
                    (Some("List"), "is_empty") => {
                        // Phase 4: from |> List/is_empty() - DD-native is_empty operation
                        match from {
                            Value::List(items) => {
                                // Check if this list came from a HOLD variable
                                let source_hold = self.get_list_source();
                                if !source_hold.name().is_empty() {
                                    // Phase 4: Use DD-native is_empty instead of ComputedRef
                                    zoon::println!("[DD_EVAL] List/is_empty() on List from HOLD: source_hold={}", source_hold);
                                    self.create_list_is_empty(source_hold.name())
                                } else {
                                    // Static is_empty for lists not from HOLD
                                    Value::Bool(items.is_empty())
                                }
                            }
                            // Phase 4: CellRef |> List/is_empty() -> DD-native is_empty
                            Value::CellRef(cell_id) => {
                                zoon::println!("[DD_EVAL] List/is_empty() on CellRef: cell_id={}", cell_id);
                                self.create_list_is_empty(cell_id.name())
                            }
                            // Phase 4: Collection |> List/is_empty() -> chain is_empty on collection
                            Value::Collection(handle) => {
                                zoon::println!("[DD_EVAL] List/is_empty() on Collection: {:?}", handle.id);
                                self.chain_is_empty_on_collection(handle.id.clone())
                            }
                            _ => Value::Bool(true),
                        }
                    }
                    // Bool operations
                    (Some("Bool"), "or") => {
                        // from |> Bool/or(that: other_bool)
                        let from_bool = match from {
                            Value::Bool(b) => *b,
                            Value::Tagged { tag, .. } => BoolTag::is_true(tag.as_ref()),
                            _ => from.is_truthy(),
                        };
                        let that_bool = args.get("that").map(|v| match v {
                            Value::Bool(b) => *b,
                            Value::Tagged { tag, .. } => BoolTag::is_true(tag.as_ref()),
                            _ => v.is_truthy(),
                        }).unwrap_or(false);
                        Value::Bool(from_bool || that_bool)
                    }
                    (Some("Bool"), "and") => {
                        // from |> Bool/and(that: other_bool)
                        let from_bool = match from {
                            Value::Bool(b) => *b,
                            Value::Tagged { tag, .. } => BoolTag::is_true(tag.as_ref()),
                            _ => from.is_truthy(),
                        };
                        let that_bool = args.get("that").map(|v| match v {
                            Value::Bool(b) => *b,
                            Value::Tagged { tag, .. } => BoolTag::is_true(tag.as_ref()),
                            _ => v.is_truthy(),
                        }).unwrap_or(true);
                        Value::Bool(from_bool && that_bool)
                    }
                    (Some("Bool"), "not") => {
                        // from |> Bool/not()
                        // Phase 4: PlaceholderField and NegatedPlaceholderField removed
                        // DD handles template substitution natively
                        let from_bool = match from {
                            Value::Bool(b) => *b,
                            Value::Tagged { tag, .. } => BoolTag::is_true(tag.as_ref()),
                            _ => from.is_truthy(),
                        };
                        Value::Bool(!from_bool)
                    }
                    // Text functions (piped)
                    (Some("Text"), "trim") => {
                        // from |> Text/trim()
                        if let Value::Text(s) = from {
                            Value::text(s.trim())
                        } else {
                            from.clone()
                        }
                    }
                    (Some("Text"), "is_not_empty") => {
                        // from |> Text/is_not_empty()
                        if let Value::Text(s) = from {
                            Value::Bool(!s.is_empty())
                        } else {
                            Value::Bool(false)
                        }
                    }
                    (Some("Text"), "is_empty") => {
                        // from |> Text/is_empty()
                        if let Value::Text(s) = from {
                            Value::Bool(s.is_empty())
                        } else {
                            Value::Bool(true)
                        }
                    }
                    // User-defined function: pass piped value as first argument
                    (None, func_name) => {
                        self.eval_user_function_with_piped(func_name, from, &args)
                    }
                    _ => self.eval_expression(to),
                }
            }

            // Pipe to HOLD - iterate if body contains Stream/pulses
            Expression::Hold { state_param, body } => {
                self.eval_hold(from, state_param.as_str(), &body.node)
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
                    // Pure DD: Use Tagged value with "__placeholder_field__" tag instead of symbolic ref
                    // The path is stored as a List of Text values for later template substitution
                    current = match &current {
                        Value::Placeholder => {
                            Value::Tagged {
                                tag: Arc::from("__placeholder_field__"),
                                fields: Arc::new(BTreeMap::from([(
                                    Arc::from("path"),
                                    Value::List(Arc::new(vec![Value::text(field.as_str())])),
                                )])),
                            }
                        }
                        Value::Tagged { tag, fields } if tag.as_ref() == "__placeholder_field__" => {
                            // Extend the existing path
                            let existing_path = fields.get("path")
                                .and_then(|v| match v {
                                    Value::List(items) => Some(items.as_ref().clone()),
                                    _ => None,
                                })
                                .unwrap_or_default();
                            let mut new_path = existing_path;
                            new_path.push(Value::text(field.as_str()));
                            Value::Tagged {
                                tag: Arc::from("__placeholder_field__"),
                                fields: Arc::new(BTreeMap::from([(
                                    Arc::from("path"),
                                    Value::List(Arc::new(new_path)),
                                )])),
                            }
                        }
                        // Handle LinkRef.event - create synthetic event object with all event types
                        Value::LinkRef(link_id) if field.as_str() == "event" => {
                            let link_ref = Value::LinkRef(link_id.clone());
                            Value::object([
                                ("press", link_ref.clone()),
                                ("click", link_ref.clone()),
                                ("blur", link_ref.clone()),
                                ("key_down", link_ref.clone()),
                                ("double_click", link_ref.clone()),
                                ("change", link_ref.clone()),
                            ])
                        }
                        _ => current.get(field.as_str()).cloned().unwrap_or(Value::Unit),
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
                zoon::println!("[DD_EVAL] LinkSetter: alias={:?} -> target_link={:?}", alias.node, target_link);

                // Replace any LinkRef in the element with the target
                let result = if let Value::LinkRef(target_id) = &target_link {
                    let result = self.replace_link_ref_in_value(from, target_id);
                    zoon::println!("[DD_EVAL] LinkSetter: replaced LinkRef with {}", target_id);
                    result
                } else if let Value::Tagged { tag, fields } = &target_link {
                    if tag.as_ref() == "__placeholder_field__" {
                        // Template evaluation: replace LinkRef with placeholder field tagged value
                        // During cloning, this will be resolved to the real LinkRef from the data item
                        let path: Vec<Arc<str>> = fields.get("path")
                            .and_then(|v| match v {
                                Value::List(items) => Some(items.iter().filter_map(|item| match item {
                                    Value::Text(t) => Some(Arc::clone(t)),
                                    _ => None,
                                }).collect()),
                                _ => None,
                            })
                            .unwrap_or_default();
                        let result = self.replace_link_ref_with_placeholder(from, &path);
                        zoon::println!("[DD_EVAL] LinkSetter: replaced LinkRef with placeholder field {:?}", path);
                        result
                    } else {
                        // Other tagged value - pass through unchanged
                        zoon::println!("[DD_EVAL] LinkSetter: tagged value not a placeholder, passing through unchanged");
                        from.clone()
                    }
                } else {
                    // If alias doesn't resolve to a LinkRef or PlaceholderField, just pass through unchanged
                    zoon::println!("[DD_EVAL] LinkSetter: alias did not resolve to LinkRef, passing through unchanged");
                    from.clone()
                };

                // Task 6.3: Detect element events AFTER LinkSetter (not during Element evaluation)
                // This ensures we capture the final link ID, not the temporary one
                if let Value::Tagged { tag, fields } = &result {
                    if ElementTag::is_element(tag.as_ref()) {
                        if let Some(Value::Text(t)) = fields.get("_element_type") {
                            let element_type = t.as_ref();
                            if let Some(element) = fields.get("element") {
                                if let Some(event) = element.get("event") {
                                    // Detect text_input key_down
                                    if element_type == "text_input" {
                                        if let Some(Value::LinkRef(link_id)) = event.get("key_down") {
                                            zoon::println!("[DD_EVAL] LinkSetter: text_input key_down detected with FINAL link: {}", link_id);
                                            super::super::io::set_text_input_key_down_link(link_id.to_string());
                                        }
                                    }
                                    // Task 6.3: Detect button press for List/clear pattern
                                    // This eliminates extract_button_press_link() document scanning
                                    if element_type == "button" {
                                        if let Some(Value::LinkRef(link_id)) = event.get("press") {
                                            zoon::println!("[DD_EVAL] LinkSetter: button press detected with FINAL link: {}", link_id);
                                            super::super::io::set_list_clear_link(link_id.to_string());
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                result
            }

            // Default
            _ => self.eval_expression(to),
        }
    }

    /// Evaluate a HOLD expression with initial value and body.
    ///
    /// For static evaluation, HOLD iterates if the body contains Stream/pulses.
    /// This enables fibonacci-style computations:
    ///   [prev: 0, curr: 1] |> HOLD state {
    ///     n-1 |> Stream/pulses() |> THEN { [prev: state.curr, curr: state.prev + state.curr] }
    ///   }
    fn eval_hold(&mut self, initial: &Value, state_name: &str, body: &Expression) -> Value {
        // Try to extract pulse count from body: `count |> Stream/pulses() |> ...`
        let pulse_count = self.extract_pulse_count(body);

        if pulse_count == 0 {
            // Check if body contains a timer trigger (variable that evaluates to TimerRef)
            // e.g., `tick |> THEN { state + 1 }` where tick = Duration |> Timer/interval()
            if let Some(interval_ms) = self.extract_timer_trigger_from_body(body) {
                let cell_id = "timer_counter";
                // NOTE: Do NOT call init_cell here - test expects empty until first tick
                zoon::println!("[DD_EVAL] Timer-triggered HOLD detected: {} @ {}ms", cell_id, interval_ms);

                // Task 4.4: Build CellConfig during evaluation (not in interpreter)
                self.add_cell_config(CellConfig {
                    id: CellId::new(cell_id),
                    initial: initial.clone(),
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
                let cell_id = self.generate_cell_id();

                // Store initial value in CELL_STATES for reactive rendering
                init_cell(&cell_id, initial.clone());

                // For boolean HOLDs, extract editing event bindings from the body
                // This parses expressions like:
                //   todo_elements.todo_title_element.event.double_click |> THEN { True }
                //   todo_elements.editing_todo_title_element.event.key_down.key |> WHEN { Enter => False }
                //   todo_elements.editing_todo_title_element.event.blur |> THEN { False }
                // Note: In Boon, True/False are tags (Tagged { tag: "False" }), not native bools
                let is_boolean_hold = matches!(initial, Value::Bool(_)) ||
                    matches!(initial, Value::Tagged { tag, .. } if BoolTag::is_bool_tag(tag.as_ref()));
                if is_boolean_hold {
                    zoon::println!("[DD_EVAL] is_boolean_hold=true for {}, extracting bindings", cell_id);
                    // Task 4.3: Use new method that evaluates to get actual LinkRef IDs
                    let mut bindings = self.extract_editing_bindings_with_link_ids(body);
                    bindings.cell_id = Some(cell_id.clone()); // Associate with this HOLD

                    // Register SetFalseOnKeys for EACH boolean HOLD (not globally)
                    // This ensures all items get their key_down handlers registered
                    if let Some(ref exit_key_link_id) = bindings.exit_key_link_id {
                        use super::super::io::{add_dynamic_link_action, DynamicLinkAction};
                        zoon::println!("[DD_EVAL] Registering SetFalseOnKeys: {} -> {}", exit_key_link_id, cell_id);
                        add_dynamic_link_action(exit_key_link_id.clone(), DynamicLinkAction::SetFalseOnKeys {
                            cell_id: cell_id.clone(),
                            keys: vec!["Enter".to_string(), "Escape".to_string()],
                        });
                    }

                    if !bindings.edit_trigger_path.is_empty() || bindings.edit_trigger_link_id.is_some() {
                        self.dataflow_config.set_editing_bindings(bindings);
                    }

                    // Also extract toggle event bindings (click |> THEN { state |> Bool/not() })
                    // Task 4.3: Use new method that evaluates to get actual LinkRef IDs
                    let toggle_bindings = self.extract_toggle_bindings_with_link_ids(body, state_name);
                    for (event_path, event_type, link_id) in toggle_bindings {
                        self.dataflow_config.add_toggle_binding(ToggleBinding {
                            cell_id: cell_id.clone(),
                            event_path,
                            event_type,
                            link_id, // Now populated with actual LinkRef ID
                        });
                    }
                }

                // For boolean HOLDs, also extract global toggle bindings
                // Pattern: store.elements.toggle_all.event.click |> THEN { store.all_completed |> Bool/not() }
                // This is in the completed HOLD inside each todo item, not in a list HOLD
                if is_boolean_hold {
                    let global_toggle_bindings = self.extract_global_toggle_bindings(body);
                    if !global_toggle_bindings.is_empty() {
                        zoon::println!("[DD_EVAL] Extracted {} global toggle bindings from boolean HOLD {}", global_toggle_bindings.len(), cell_id);
                        for (event_path, event_type, value_path) in global_toggle_bindings {
                            zoon::println!("[DD_EVAL] Adding global toggle binding: event_path={:?}, value_path={:?}", event_path, value_path);
                            // For boolean HOLDs, the list_cell_id is the HOLD ID of this completed field
                            // The action will toggle this specific HOLD based on store.all_completed
                            self.dataflow_config.add_global_toggle_binding(GlobalToggleBinding {
                                list_cell_id: cell_id.clone(),  // This is the completed HOLD, not a list
                                event_path,
                                event_type,
                                value_path,
                            });
                        }
                    }
                }

                zoon::println!("[DD_EVAL] LINK-triggered HOLD detected: {} with initial {:?}", cell_id, initial);

                // Task 4.4: Build CellConfig during evaluation
                // Determine the transform from the body pattern
                let transform = self.determine_transform(body, state_name, initial);
                zoon::println!("[DD_EVAL] Determined transform: {:?}", transform);

                // Task 7.1: Extract trigger LinkId from body dynamically (no hardcoded fallbacks)
                let triggered_by = self.extract_link_trigger_id(body)
                    .map(|id| vec![LinkId::new(&id)])
                    .unwrap_or_default();
                zoon::println!("[DD_EVAL] CellConfig triggered_by: {:?}", triggered_by);

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
                    persist: !is_boolean_hold || matches!(initial, Value::Bool(_)), // Don't persist editing state
                });

                // Also register DynamicLinkAction for SetTrue/SetFalse transforms
                // This enables replication when list items are cloned
                use super::super::core::worker::StateTransform;
                use super::super::io::{add_dynamic_link_action, DynamicLinkAction};
                for link_id in &triggered_by_clone {
                    match transform_clone {
                        StateTransform::SetTrue => {
                            add_dynamic_link_action(link_id.name().to_string(), DynamicLinkAction::SetTrue(cell_id.clone()));
                        }
                        StateTransform::SetFalse => {
                            add_dynamic_link_action(link_id.name().to_string(), DynamicLinkAction::SetFalse(cell_id.clone()));
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
            let mut iter_runtime = BoonDdRuntime {
                variables: self.variables.clone(),
                functions: self.functions.clone(),
                passed_context: self.passed_context.clone(),
                link_counter: 0,
                cell_counter: 0,
                context_path: Vec::new(),
                last_list_source: None,
                dataflow_config: DataflowConfig::new(), // Iteration doesn't accumulate config
                cell_to_collection: HashMap::new(),
            };
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
                        return (n.0 as i64).max(0);
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

    /// Get the source name for a list, tracked during evaluation.
    /// This returns the variable/field name that the list was accessed from.
    /// Used to set source_hold for ReactiveFilteredList.
    fn get_list_source(&self) -> CellId {
        match &self.last_list_source {
            Some(name) => CellId::new(name.as_str()),
            None => CellId::new(""),
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

    /// Extract editing event bindings from a HOLD body.
    /// Parses expressions like:
    /// ```boon
    /// LATEST {
    ///     todo_elements.todo_title_element.event.double_click |> THEN { True }
    ///     todo_elements.editing_todo_title_element.event.key_down.key |> WHEN { Enter => False }
    ///     todo_elements.editing_todo_title_element.event.blur |> THEN { False }
    /// }
    /// ```
    /// Returns EditingEventBindings with paths extracted from the event expressions.
    fn extract_editing_bindings(&self, body: &Expression) -> super::super::io::EditingEventBindings {
        let mut bindings = super::super::io::EditingEventBindings::default();

        // Helper to extract inputs from LATEST
        let inputs = match body {
            Expression::Latest { inputs, .. } => inputs.iter().map(|s| &s.node).collect::<Vec<_>>(),
            _ => vec![body], // Single expression, not wrapped in LATEST
        };

        for input in &inputs {
            // Look for patterns like: path.event.X |> THEN/WHEN { value }
            if let Some((event_path, event_type, result_value)) = self.extract_event_binding(input) {
                match (event_type.as_str(), result_value) {
                    // double_click |> THEN { True } → edit trigger
                    ("double_click", Some(true)) => {
                        bindings.edit_trigger_path = event_path;
                    }
                    // key_down |> WHEN { Enter => False, Escape => False } → exit on key
                    ("key_down", Some(false)) => {
                        bindings.exit_key_path = event_path;
                    }
                    // blur |> THEN { False } → exit on blur
                    ("blur", Some(false)) => {
                        bindings.exit_blur_path = event_path;
                    }
                    _ => {}
                }
            }
        }

        bindings
    }

    /// Extract editing bindings with actual LinkRef IDs by evaluating the event source expressions.
    ///
    /// This method EVALUATES the `from` expression in editing patterns to get actual LinkRef IDs.
    /// Used for Task 4.3: eliminate interpreter's extract_editing_toggles dependency.
    fn extract_editing_bindings_with_link_ids(&mut self, body: &Expression) -> EditingBinding {
        let mut bindings = EditingBinding::default();

        // Helper to extract inputs from LATEST
        let inputs = match body {
            Expression::Latest { inputs, .. } => inputs.iter().map(|s| &s.node).collect::<Vec<_>>(),
            _ => vec![body],
        };

        for input in inputs {
            // Look for patterns like: path.event.X |> THEN/WHEN { value }
            if let Expression::Pipe { from, to } = input {
                // Extract path and event type
                let path_info = self.extract_event_path(&from.node);
                let (event_type, result_value) = match &to.node {
                    Expression::Then { body: then_body } => {
                        let result = self.extract_bool_literal(&then_body.node);
                        ("then".to_string(), result)
                    }
                    Expression::When { arms } => {
                        // Check if arms produce False for Enter/Escape
                        let has_false_on_keys = arms.iter().any(|arm| {
                            if let Pattern::Literal(Literal::Tag(name)) = &arm.pattern {
                                (name.as_ref() == "Enter" || name.as_ref() == "Escape") && self.extract_bool_literal(&arm.body.node) == Some(false)
                            } else {
                                false
                            }
                        });
                        if has_false_on_keys {
                            ("when_false".to_string(), Some(false))
                        } else {
                            continue;
                        }
                    }
                    _ => continue,
                };

                // CRITICAL: Evaluate the `from` expression to get actual LinkRef ID
                // Note: For key_down, the pattern is `path.event.key_down.key |> WHEN {...}`
                // We need to evaluate `path.event.key_down` (the LinkRef), not `path.event.key_down.key`
                let from_value = self.eval_expression(&from.node);
                let link_id = if let Value::LinkRef(id) = &from_value {
                    Some(id.to_string())
                } else if let Some((path, _)) = &path_info {
                    // If direct evaluation didn't return a LinkRef (e.g., key_down.key case),
                    // the path already points to the element containing the LINK.
                    // The `.event.key_down` part is syntactic sugar - the LinkRef is at the path itself.
                    // Resolve just the path to get the LinkRef.
                    if let Some(val) = self.resolve_field_path(path) {
                        if let Value::LinkRef(id) = val {
                            Some(id.to_string())
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                if let Some((path, actual_event_type)) = path_info {
                    match (actual_event_type.as_str(), event_type.as_str(), result_value) {
                        // double_click |> THEN { True } → edit trigger
                        ("double_click", "then", Some(true)) => {
                            zoon::println!("[DD_EVAL] edit_trigger: path={:?}, link_id={:?}", path, link_id);
                            bindings.edit_trigger_path = path;
                            bindings.edit_trigger_link_id = link_id;
                        }
                        // key_down |> WHEN { Enter => False } → exit on key
                        ("key_down", "when_false", Some(false)) => {
                            zoon::println!("[DD_EVAL] exit_key: path={:?}, link_id={:?}", path, link_id);
                            bindings.exit_key_path = path;
                            bindings.exit_key_link_id = link_id;
                        }
                        // blur |> THEN { False } → exit on blur
                        ("blur", "then", Some(false)) => {
                            zoon::println!("[DD_EVAL] exit_blur: path={:?}, link_id={:?}", path, link_id);
                            bindings.exit_blur_path = path;
                            bindings.exit_blur_link_id = link_id;
                        }
                        _ => {}
                    }
                }
            }
        }

        bindings
    }

    /// Extract event binding from an expression like `path.event.X |> THEN/WHEN { value }`.
    /// Returns (path_to_linkref, event_type, result_bool_value).
    fn extract_event_binding(&self, expr: &Expression) -> Option<(Vec<String>, String, Option<bool>)> {
        match expr {
            Expression::Pipe { from, to } => {
                // Check if piping to THEN { True/False }
                if let Expression::Then { body } = &to.node {
                    if let Some((path, event_type)) = self.extract_event_path(&from.node) {
                        let result_value = self.extract_bool_literal(&body.node);
                        return Some((path, event_type, result_value));
                    }
                }
                // Check if piping to WHEN { Enter => False, ... }
                if let Expression::When { arms } = &to.node {
                    if let Some((path, event_type)) = self.extract_event_path(&from.node) {
                        // Check if arms produce False for Enter/Escape
                        let has_false_on_keys = arms.iter().any(|arm| {
                            // Tags in WHEN arms are Literal::Tag, not TaggedObject
                            if let Pattern::Literal(Literal::Tag(name)) = &arm.pattern {
                                (name.as_ref() == "Enter" || name.as_ref() == "Escape") && self.extract_bool_literal(&arm.body.node) == Some(false)
                            } else {
                                false
                            }
                        });
                        if has_false_on_keys {
                            return Some((path, event_type, Some(false)));
                        }
                    }
                }
                // Recurse into nested pipes
                self.extract_event_binding(&from.node)
            }
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
        // Start by looking up the first part in variables
        let first = &path[0];
        let mut current = self.variables.get(first.as_str())?.clone();

        // Traverse the remaining path
        for part in &path[1..] {
            current = current.get(part.as_str())?.clone();
        }

        Some(current)
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
                            _ => {}
                        }
                    }
                }
                false
            }
            _ => false,
        }
    }

    /// Check if expression is a global toggle pattern like `store.all_completed |> Bool/not()`.
    /// Returns Some(path) where path is the alias parts (e.g., ["store", "all_completed"]).
    fn is_global_toggle_pattern(&self, expr: &Expression) -> Option<Vec<String>> {
        match expr {
            Expression::Pipe { from, to } => {
                // Check: X |> Bool/not() where X is an alias (not state)
                if let Expression::FunctionCall { path, .. } = &to.node {
                    let fn_path: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                    if fn_path == vec!["Bool", "not"] {
                        // Check if from is an alias (like store.all_completed)
                        match &from.node {
                            Expression::Alias(Alias::WithoutPassed { parts, .. }) => {
                                // Multi-part alias like store.all_completed
                                if parts.len() > 1 && parts[0].as_ref() == "store" {
                                    return Some(parts.iter().map(|p| p.to_string()).collect());
                                }
                            }
                            Expression::Alias(Alias::WithPassed { extra_parts, .. }) => {
                                // PASSED.store.X - also a global reference
                                if !extra_parts.is_empty() && extra_parts[0].as_ref() == "store" {
                                    return Some(extra_parts.iter().map(|p| p.to_string()).collect());
                                }
                            }
                            _ => {}
                        }
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Extract global toggle bindings from HOLD body.
    /// Looks for patterns like: `store.elements.X.event.click |> THEN { store.Y |> Bool/not() }`
    /// Returns list of (event_path, event_type, value_path) for global toggles.
    fn extract_global_toggle_bindings(&self, body: &Expression) -> Vec<(Vec<String>, String, Vec<String>)> {
        zoon::println!("[DD_EVAL] extract_global_toggle_bindings CALLED");
        let mut toggles = Vec::new();

        // Helper to extract inputs from LATEST
        let inputs = match body {
            Expression::Latest { inputs, .. } => {
                inputs.iter().map(|s| &s.node).collect::<Vec<_>>()
            }
            _ => vec![body]
        };

        for input in inputs.iter() {
            // Look for patterns like: store.elements.X.event.click |> THEN { store.Y |> Bool/not() }
            if let Expression::Pipe { from, to } = input {
                if let Expression::Then { body: then_body } = &to.node {
                    // Check if THEN body is a global toggle (store.X |> Bool/not())
                    if let Some(value_path) = self.is_global_toggle_pattern(&then_body.node) {
                        zoon::println!("[DD_EVAL] extract_global_toggle_bindings: found global toggle with value_path {:?}", value_path);
                        // Extract the event path from the from side
                        if let Some((path, event_type)) = self.extract_event_path(&from.node) {
                            zoon::println!("[DD_EVAL] extract_global_toggle_bindings: extracted event path {:?}, type {}", path, event_type);
                            toggles.push((path, event_type, value_path));
                        }
                    }
                }
            }
        }

        toggles
    }

    /// Extract checkbox toggle bindings from HOLD body.
    /// Looks for patterns like: `path.event.click |> THEN { state |> Bool/not() }`
    /// Returns list of (event_path, event_type) for toggles.
    fn extract_toggle_bindings(&self, body: &Expression, state_name: &str) -> Vec<(Vec<String>, String)> {
        zoon::println!("[DD_EVAL] extract_toggle_bindings CALLED with state_name={}", state_name);
        let mut toggles = Vec::new();

        // Helper to extract inputs from LATEST
        let inputs = match body {
            Expression::Latest { inputs, .. } => {
                zoon::println!("[DD_EVAL] extract_toggle_bindings: found LATEST with {} inputs", inputs.len());
                inputs.iter().map(|s| &s.node).collect::<Vec<_>>()
            }
            _ => {
                zoon::println!("[DD_EVAL] extract_toggle_bindings: body is not LATEST, treating as single input");
                vec![body]
            }
        };

        for (i, input) in inputs.iter().enumerate() {
            // Look for patterns like: path.event.click |> THEN { state |> Bool/not() }
            if let Expression::Pipe { from, to } = input {
                if let Expression::Then { body: then_body } = &to.node {
                    zoon::println!("[DD_EVAL] extract_toggle_bindings: input {} is Pipe->THEN, checking for Bool/not pattern with state_name={}", i, state_name);
                    // Check if THEN body is Bool/not() toggle
                    if self.is_bool_toggle_pattern(&then_body.node, state_name) {
                        zoon::println!("[DD_EVAL] extract_toggle_bindings: input {} IS Bool/not toggle!", i);
                        // Extract the event path from the from side
                        if let Some((path, event_type)) = self.extract_event_path(&from.node) {
                            zoon::println!("[DD_EVAL] extract_toggle_bindings: extracted path {:?}, event_type {}", path, event_type);
                            toggles.push((path, event_type));
                        } else {
                            zoon::println!("[DD_EVAL] extract_toggle_bindings: failed to extract event path");
                        }
                    } else {
                        zoon::println!("[DD_EVAL] extract_toggle_bindings: input {} is NOT Bool/not toggle", i);
                    }
                }
            }
        }

        toggles
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
                            zoon::println!("[DD_EVAL] extract_toggle_bindings_with_link_ids: evaluated to LinkRef({})", id);
                            Some(id.to_string())
                        } else {
                            zoon::println!("[DD_EVAL] extract_toggle_bindings_with_link_ids: from evaluated to {:?}, not LinkRef", from_value);
                            None
                        };

                        if let Some((path, event_type)) = path_info {
                            toggles.push((path, event_type, link_id));
                        } else if let Some(ref link_id) = link_id {
                            // Have LinkRef ID but no path - use link_id as fallback path
                            toggles.push((vec![link_id.clone()], "click".to_string(), Some(link_id.clone())));
                        }
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
                        zoon::println!("[DD_EVAL] extract_link_trigger_id: found LinkRef({})", id);
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
                        let interval_ms = match &duration {
                            Value::Tagged { tag, fields } if tag.as_ref() == "Duration" => {
                                if let Some(Value::Number(secs)) = fields.get("seconds") {
                                    (secs.0 * 1000.0) as u64
                                } else if let Some(Value::Number(ms)) = fields.get("millis") {
                                    ms.0 as u64
                                } else {
                                    1000
                                }
                            }
                            Value::Number(ms) => ms.0 as u64,
                            _ => 1000,
                        };
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
    fn extract_router_mappings(&mut self, expr: &Expression) {
        // Phase 11a: Router mappings now go through DD dataflow
        // This function logs mappings for debugging but doesn't add to global state
        if let Expression::Latest { inputs } = expr {
            for input in inputs {
                // Each input should be: alias.event.press |> THEN { route_text }
                if let Expression::Pipe { from, to } = &input.node {
                    if let Expression::Then { body } = &to.node {
                        // Extract the LinkRef from the FROM side
                        // For `nav.about.event.press`, we need to get `nav.about` and resolve it
                        let link_ref = self.extract_link_ref_from_event_path(&from.node);
                        // Extract the route from the body (e.g., TEXT { /about })
                        let route = self.eval_expression(&body.node);

                        if let (Some(link_id), Value::Text(route_text)) = (link_ref, route) {
                            // Phase 11a: Routing now goes through DD - just log for debugging
                            zoon::println!("[DD_EVAL] Router mapping (DD-routed): link={} -> route={}", link_id, route_text);
                        }
                    }
                }
            }
        }
    }

    /// Extract a LinkRef ID from an event path like `nav.about.event.press`.
    ///
    /// The path `nav.about.event.press` means:
    /// - `nav.about` is the LINK variable (LinkRef)
    /// - `.event.press` is the event type (we strip this)
    ///
    /// We evaluate `nav.about` to get the LinkRef and return its ID.
    fn extract_link_ref_from_event_path(&mut self, expr: &Expression) -> Option<String> {
        match expr {
            Expression::Alias(Alias::WithoutPassed { parts, .. }) => {
                // Path like ["nav", "about", "event", "press"]
                // Find where ".event.press" starts and take everything before it
                let parts_vec: Vec<&str> = parts.iter().map(|s| s.as_str()).collect();

                // Find the "event" part and take path up to (but not including) it
                let event_idx = parts_vec.iter().position(|&p| p == "event")?;
                if event_idx == 0 {
                    return None; // No link path before "event"
                }

                // Build the link path (everything before "event")
                let link_path: Vec<&str> = parts_vec[..event_idx].to_vec();

                // Evaluate the link path to get the LinkRef
                let mut current = self.variables.get(link_path[0])?.clone();
                for field in link_path.iter().skip(1) {
                    current = current.get(field).cloned().unwrap_or(Value::Unit);
                }

                // Extract the link ID from the LinkRef
                if let Value::LinkRef(id) = current {
                    Some(id.to_string())
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Evaluate LATEST expression - merge multiple reactive inputs.
    ///
    /// LATEST { a, b, c } semantics:
    /// - Static inputs (no THEN) are initial values
    /// - Event-driven inputs (X |> THEN { Y }) are reactive triggers
    /// - Returns LatestRef if any inputs are event-driven
    /// - Returns first non-Unit value if all inputs are static
    fn eval_latest(&mut self, inputs: &[Spanned<Expression>]) -> Value {
        let mut initial_value = Value::Unit;
        let mut events = Vec::new();
        let mut event_values = Vec::new();
        let mut has_events = false;

        for input in inputs {
            // Check if this input contains a THEN pattern (event-driven)
            if self.contains_then_pattern(&input.node) {
                has_events = true;
                // Extract event source and value from pattern like `link |> THEN { value }`
                if let Some((event_source, event_value)) = self.extract_event_and_value(&input.node) {
                    events.push(event_source);
                    event_values.push(event_value);
                }
            } else {
                // Static input - evaluate and use as initial if we haven't found one yet
                let val = self.eval_expression(&input.node);
                if initial_value == Value::Unit && val != Value::Unit {
                    initial_value = val;
                }
            }
        }

        if has_events {
            // Pure DD: LATEST with events requires DD merge operator integration
            // For now, create a cell and return CellRef for basic reactivity
            // Full merge behavior will be added when DD LATEST operator is implemented
            let cell_id = format!("latest_{}", GLOBAL_CELL_COUNTER.fetch_add(1, Ordering::SeqCst));

            // Initialize the cell with the initial value
            init_cell(&cell_id, initial_value.clone());

            // For each event source, configure DD to update this cell
            // Note: Full LATEST merge semantics require a dedicated DD operator
            for event in events.iter() {
                match event {
                    Value::LinkRef(link_id) => {
                        // Add cell config for this event trigger using Identity transform
                        // The event value becomes the cell state
                        self.dataflow_config.cells.push(CellConfig {
                            id: CellId::new(&cell_id),
                            initial: initial_value.clone(),
                            triggered_by: vec![LinkId::new(&link_id.name())],
                            timer_interval_ms: 0,
                            filter: EventFilter::Any,
                            transform: StateTransform::Identity,
                            persist: false,
                        });
                    }
                    Value::TimerRef { id, interval_ms } => {
                        // Timer-triggered update
                        self.dataflow_config.cells.push(CellConfig {
                            id: CellId::new(&cell_id),
                            initial: initial_value.clone(),
                            triggered_by: vec![],
                            timer_interval_ms: *interval_ms,
                            filter: EventFilter::Any,
                            transform: StateTransform::Identity,
                            persist: false,
                        });
                    }
                    _ => {}
                }
            }

            zoon::println!("[DD_EVAL] LATEST with events: cell={}, initial={:?}, events={:?}", cell_id, initial_value, events.len());
            Value::CellRef(CellId::new(&cell_id))
        } else {
            // All static - return first non-Unit value (current behavior)
            initial_value
        }
    }

    /// Extract event source (LinkRef/TimerRef) and emitted value from a THEN pattern.
    /// For `link |> THEN { value }`, returns (LinkRef, evaluated_value).
    fn extract_event_and_value(&mut self, expr: &Expression) -> Option<(Value, Value)> {
        match expr {
            Expression::Pipe { from, to } => {
                if let Expression::Then { body } = &to.node {
                    // Evaluate the source to get LinkRef/TimerRef
                    let source = self.eval_expression(&from.node);
                    // Evaluate the body to get the emitted value
                    let value = self.eval_expression(&body.node);

                    match &source {
                        Value::LinkRef(_) | Value::TimerRef { .. } => {
                            return Some((source, value));
                        }
                        _ => {
                            // Source might be a path like button.event.press
                            // Try to extract LinkRef from nested structure
                            if let Some(link_ref) = self.try_extract_link_ref(&source) {
                                return Some((link_ref, value));
                            }
                        }
                    }
                }
                // Recurse into nested pipes
                self.extract_event_and_value(&to.node)
            }
            _ => None,
        }
    }

    /// Try to extract a LinkRef from a nested object structure.
    /// For paths like `button.event.press`, the evaluated result might be
    /// an Object containing a LinkRef.
    fn try_extract_link_ref(&self, value: &Value) -> Option<Value> {
        match value {
            Value::LinkRef(_) => Some(value.clone()),
            Value::Object(fields) => {
                // Check common event field names
                for key in ["press", "click", "change", "key_down", "blur", "double_click"] {
                    if let Some(v) = fields.get(key) {
                        if let Value::LinkRef(_) = v {
                            return Some(v.clone());
                        }
                    }
                }
                // Check nested event object
                if let Some(Value::Object(event_fields)) = fields.get("event") {
                    for (_, v) in event_fields.iter() {
                        if let Value::LinkRef(_) = v {
                            return Some(v.clone());
                        }
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Evaluate pattern matching for WHEN/WHILE.
    fn eval_pattern_match(&mut self, value: &Value, arms: &[Arm]) -> Value {
        // Debug: log what value type is being pattern matched
        zoon::println!("[DD_EVAL] eval_pattern_match input: {:?}", value);

        // If input is a CellRef, return a WhileRef for reactive rendering
        if let Value::CellRef(cell_id) = value {
            // Pre-evaluate all arms for the bridge to render reactively
            // Note: WHILE_PREEVAL_DEPTH hack was removed (Phase 11b) - fine-grained signals
            // from cell_signal() prevent spurious side effects during pre-evaluation
            let mut evaluated_arms = Vec::new();
            let mut default_value = None;

            for arm in arms {
                // Extract the pattern value (tag or literal) for matching
                let pattern_value = self.pattern_to_value(&arm.pattern);

                // Check if this is a wildcard (default) pattern
                if matches!(arm.pattern, Pattern::WildCard) {
                    // Evaluate the body for the default case
                    let mut match_runtime = BoonDdRuntime {
                        variables: self.variables.clone(),
                        functions: self.functions.clone(),
                        passed_context: self.passed_context.clone(),
                        link_counter: 0,
                        cell_counter: 0,
                        context_path: Vec::new(),
                        last_list_source: None,
                    dataflow_config: DataflowConfig::new(),
                    cell_to_collection: HashMap::new(),
                    };
                    let body_result = match_runtime.eval_expression(&arm.body.node);
                    default_value = Some(Arc::new(body_result));
                } else if let Pattern::Alias { name } = &arm.pattern {
                    // Alias pattern - treat as catch-all but bind the CellRef to the alias name
                    let mut match_runtime = BoonDdRuntime {
                        variables: self.variables.clone(),
                        functions: self.functions.clone(),
                        passed_context: self.passed_context.clone(),
                        link_counter: 0,
                        cell_counter: 0,
                        context_path: Vec::new(),
                        last_list_source: None,
                    dataflow_config: DataflowConfig::new(),
                    cell_to_collection: HashMap::new(),
                    };
                    // Bind the alias name to the CellRef value
                    match_runtime.variables.insert(name.to_string(), value.clone());
                    zoon::println!("[DD_EVAL] CellRef WHEN: binding '{}' to CellRef for body evaluation", name);
                    let body_result = match_runtime.eval_expression(&arm.body.node);
                    default_value = Some(Arc::new(body_result));
                } else if let Some(pv) = pattern_value {
                    // Evaluate the body for this pattern
                    let mut match_runtime = BoonDdRuntime {
                        variables: self.variables.clone(),
                        functions: self.functions.clone(),
                        passed_context: self.passed_context.clone(),
                        link_counter: 0,
                        cell_counter: 0,
                        context_path: Vec::new(),
                        last_list_source: None,
                    dataflow_config: DataflowConfig::new(),
                    cell_to_collection: HashMap::new(),
                    };
                    let body_result = match_runtime.eval_expression(&arm.body.node);
                    evaluated_arms.push((pv, body_result));
                }
            }

            zoon::println!("[DD_EVAL] Created WHILE config for hold {} with {} arms", cell_id, evaluated_arms.len());

            // Pure DD: Return Tagged value with WHILE configuration
            // The bridge interprets this to render conditionally based on cell value
            let arms_list: Vec<Value> = evaluated_arms.into_iter()
                .map(|(pattern, body)| Value::object([
                    ("pattern", pattern),
                    ("body", body),
                ]))
                .collect();

            return Value::Tagged {
                tag: Arc::from("__while_config__"),
                fields: Arc::new(BTreeMap::from([
                    (Arc::from("cell_id"), Value::text(&cell_id.name())),
                    (Arc::from("computation"), Value::Unit),  // No computation for direct CellRef
                    (Arc::from("arms"), Value::List(Arc::new(arms_list))),
                    (Arc::from("default"), default_value.map(|v| (*v).clone()).unwrap_or(Value::Unit)),
                ])),
            };
        }

        // If input is a placeholder field tagged value, create placeholder WHILE config
        // This handles: todo.editing |> WHILE { True => ..., False => ... } in templates
        if let Value::Tagged { tag, fields } = value {
            if tag.as_ref() == "__placeholder_field__" {
                let path = fields.get("path")
                    .and_then(|v| match v {
                        Value::List(items) => Some(items.clone()),
                        _ => None,
                    })
                    .unwrap_or_else(|| Arc::new(vec![]));

            // Pre-evaluate all arms for later substitution
            // Note: WHILE_PREEVAL_DEPTH hack was removed (Phase 11b) - fine-grained signals
            // from cell_signal() prevent spurious side effects during pre-evaluation
            let mut evaluated_arms = Vec::new();
            let mut default_value = None;

            for arm in arms {
                let pattern_value = self.pattern_to_value(&arm.pattern);

                if matches!(arm.pattern, Pattern::WildCard) {
                    let mut match_runtime = BoonDdRuntime {
                        variables: self.variables.clone(),
                        functions: self.functions.clone(),
                        passed_context: self.passed_context.clone(),
                        link_counter: 0,
                        cell_counter: 0,
                        context_path: Vec::new(),
                        last_list_source: None,
                    dataflow_config: DataflowConfig::new(),
                    cell_to_collection: HashMap::new(),
                    };
                    let body_result = match_runtime.eval_expression(&arm.body.node);
                    default_value = Some(Arc::new(body_result));
                } else if let Some(pv) = pattern_value {
                    let mut match_runtime = BoonDdRuntime {
                        variables: self.variables.clone(),
                        functions: self.functions.clone(),
                        passed_context: self.passed_context.clone(),
                        link_counter: 0,
                        cell_counter: 0,
                        context_path: Vec::new(),
                        last_list_source: None,
                    dataflow_config: DataflowConfig::new(),
                    cell_to_collection: HashMap::new(),
                    };
                    let body_result = match_runtime.eval_expression(&arm.body.node);
                    evaluated_arms.push((pv, body_result));
                }
            }

            zoon::println!("[DD_EVAL] Created placeholder WHILE config for path with {} arms", evaluated_arms.len());

            // Pure DD: Return Tagged value with placeholder WHILE configuration
            // This is resolved during template instantiation
            let arms_list: Vec<Value> = evaluated_arms.into_iter()
                .map(|(pattern, body)| Value::object([
                    ("pattern", pattern),
                    ("body", body),
                ]))
                .collect();

            return Value::Tagged {
                tag: Arc::from("__placeholder_while__"),
                fields: Arc::new(BTreeMap::from([
                    (Arc::from("field_path"), Value::List(path)),
                    (Arc::from("arms"), Value::List(Arc::new(arms_list))),
                    (Arc::from("default"), default_value.map(|v| (*v).clone()).unwrap_or(Value::Unit)),
                ])),
            };
            }
        }

        // NOTE: ComputedRef handling was removed in pure DD migration
        // Boolean computations should go through DD operators instead

        // If input is a LinkRef (e.g., element.hovered), create a synthetic hold for boolean state
        // This handles: element.hovered |> WHILE { True => delete_button, False => NoElement }
        if let Value::LinkRef(link_id) = value {
            // Create a synthetic hold for this link's boolean state
            let cell_id = format!("hover_{}", link_id);

            // Initialize the hold state to False (not hovered initially)
            init_cell(&cell_id, Value::Bool(false));

            // Pre-evaluate all arms for the bridge to render reactively
            let mut evaluated_arms = Vec::new();
            let mut default_value = None;

            for arm in arms {
                let pattern_value = self.pattern_to_value(&arm.pattern);

                if matches!(arm.pattern, Pattern::WildCard) {
                    let mut match_runtime = BoonDdRuntime {
                        variables: self.variables.clone(),
                        functions: self.functions.clone(),
                        passed_context: self.passed_context.clone(),
                        link_counter: 0,
                        cell_counter: 0,
                        context_path: Vec::new(),
                        last_list_source: None,
                    dataflow_config: DataflowConfig::new(),
                    cell_to_collection: HashMap::new(),
                    };
                    let body_result = match_runtime.eval_expression(&arm.body.node);
                    default_value = Some(Arc::new(body_result));
                } else if let Some(pv) = pattern_value {
                    let mut match_runtime = BoonDdRuntime {
                        variables: self.variables.clone(),
                        functions: self.functions.clone(),
                        passed_context: self.passed_context.clone(),
                        link_counter: 0,
                        cell_counter: 0,
                        context_path: Vec::new(),
                        last_list_source: None,
                    dataflow_config: DataflowConfig::new(),
                    cell_to_collection: HashMap::new(),
                    };
                    let body_result = match_runtime.eval_expression(&arm.body.node);
                    evaluated_arms.push((pv, body_result));
                }
            }

            // Log what the True arm contains (specifically look for button press LinkRef)
            for (pattern, body) in &evaluated_arms {
                if matches!(pattern, Value::Tagged { tag, .. } if BoolTag::is_true(tag.as_ref())) {
                    // Check if body contains a button with a press LinkRef
                    fn find_press_link(v: &Value) -> Option<String> {
                        match v {
                            Value::Tagged { tag, fields } if ElementTag::is_element(tag.as_ref()) => {
                                if let Some(element) = fields.get("element") {
                                    if let Some(event) = element.get("event") {
                                        if let Some(Value::LinkRef(id)) = event.get("press") {
                                            return Some(id.to_string());
                                        }
                                    }
                                }
                                // Recurse into items
                                if let Some(Value::List(items)) = fields.get("items") {
                                    for item in items.iter() {
                                        if let Some(link) = find_press_link(item) {
                                            return Some(link);
                                        }
                                    }
                                }
                                None
                            }
                            Value::Object(obj) => {
                                if let Some(event) = obj.get("event") {
                                    if let Some(Value::LinkRef(id)) = event.get("press") {
                                        return Some(id.to_string());
                                    }
                                }
                                None
                            }
                            _ => None,
                        }
                    }
                    if let Some(press_link) = find_press_link(body) {
                        zoon::println!("[DD_EVAL] WhileRef {} True arm button press: {}", cell_id, press_link);
                    }
                }
            }

            zoon::println!("[DD_EVAL] Created WHILE config for LinkRef {} (hover hold: {}) with {} arms", link_id, cell_id, evaluated_arms.len());

            // Pure DD: Return Tagged value with WHILE configuration
            let arms_list: Vec<Value> = evaluated_arms.into_iter()
                .map(|(pattern, body)| Value::object([
                    ("pattern", pattern),
                    ("body", body),
                ]))
                .collect();

            return Value::Tagged {
                tag: Arc::from("__while_config__"),
                fields: Arc::new(BTreeMap::from([
                    (Arc::from("cell_id"), Value::text(&cell_id)),
                    (Arc::from("computation"), Value::Unit),  // No computation - just read hold state directly
                    (Arc::from("arms"), Value::List(Arc::new(arms_list))),
                    (Arc::from("default"), default_value.map(|v| (*v).clone()).unwrap_or(Value::Unit)),
                ])),
            };
        }

        // If input is a WHILE config (Tagged), chain the pattern matching
        // This happens when: route |> WHEN { "/" => Home } |> WHILE { Home => page(...) }
        if let Value::Tagged { tag, fields } = value {
            if tag.as_ref() == "__while_config__" {
                let cell_id = fields.get("cell_id")
                    .and_then(|v| v.as_text())
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                let input_computation = fields.get("computation").cloned().unwrap_or(Value::Unit);
                let input_arms: Vec<(Value, Value)> = fields.get("arms")
                    .and_then(|v| match v {
                        Value::List(items) => Some(items.iter().filter_map(|item| {
                            let pattern = item.get("pattern").cloned()?;
                            let body = item.get("body").cloned()?;
                            Some((pattern, body))
                        }).collect()),
                        _ => None,
                    })
                    .unwrap_or_default();
                let input_default = fields.get("default").cloned().filter(|v| *v != Value::Unit);
            // For each arm in this WHILE, evaluate the body for each possible input value
            // This creates a composed transformation: route → page tag → page element
            let mut evaluated_arms = Vec::new();
            let mut default_value = None;

            // For each input arm (e.g., "/" → Home), find the corresponding output
            for (input_pattern, intermediate_value) in input_arms.iter() {
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
                            let mut match_runtime = BoonDdRuntime {
                                variables: self.variables.clone(),
                                functions: self.functions.clone(),
                                passed_context: self.passed_context.clone(),
                                link_counter: 0,
                                cell_counter: 0,
                                context_path: Vec::new(),
                                last_list_source: None,
                            dataflow_config: DataflowConfig::new(),
                            cell_to_collection: HashMap::new(),
                            };
                            let body_result = match_runtime.eval_expression(&arm.body.node);
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
                        let mut match_runtime = BoonDdRuntime {
                            variables: self.variables.clone(),
                            functions: self.functions.clone(),
                            passed_context: self.passed_context.clone(),
                            link_counter: 0,
                            cell_counter: 0,
                            context_path: Vec::new(),
                            last_list_source: None,
                        dataflow_config: DataflowConfig::new(),
                        cell_to_collection: HashMap::new(),
                        };
                        let body_result = match_runtime.eval_expression(&arm.body.node);
                        default_value = Some(Arc::new(body_result));
                        break;
                    }

                    let pattern_value = self.pattern_to_value(&arm.pattern);
                    if let Some(pv) = &pattern_value {
                        let matches = match (input_def.as_ref(), pv) {
                            (Value::Tagged { tag: a, .. }, Value::Tagged { tag: b, .. }) => a == b,
                            _ => input_def.as_ref() == pv,
                        };
                        if matches {
                            let mut match_runtime = BoonDdRuntime {
                                variables: self.variables.clone(),
                                functions: self.functions.clone(),
                                passed_context: self.passed_context.clone(),
                                link_counter: 0,
                                cell_counter: 0,
                                context_path: Vec::new(),
                                last_list_source: None,
                            dataflow_config: DataflowConfig::new(),
                            cell_to_collection: HashMap::new(),
                            };
                            let body_result = match_runtime.eval_expression(&arm.body.node);
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
                        let mut match_runtime = BoonDdRuntime {
                            variables: self.variables.clone(),
                            functions: self.functions.clone(),
                            passed_context: self.passed_context.clone(),
                            link_counter: 0,
                            cell_counter: 0,
                            context_path: Vec::new(),
                            last_list_source: None,
                        dataflow_config: DataflowConfig::new(),
                        cell_to_collection: HashMap::new(),
                        };
                        let body_result = match_runtime.eval_expression(&arm.body.node);
                        default_value = Some(Arc::new(body_result));
                        break;
                    }
                }
            }

            zoon::println!("[DD_EVAL] Chained WHILE config for cell {} with {} arms", cell_id, evaluated_arms.len());

            // Pure DD: Return Tagged value with chained WHILE configuration
            let arms_list: Vec<Value> = evaluated_arms.into_iter()
                .map(|(pattern, body)| Value::object([
                    ("pattern", pattern),
                    ("body", body),
                ]))
                .collect();

            return Value::Tagged {
                tag: Arc::from("__while_config__"),
                fields: Arc::new(BTreeMap::from([
                    (Arc::from("cell_id"), Value::text(&cell_id)),
                    (Arc::from("computation"), input_computation),  // Preserve computation from input
                    (Arc::from("arms"), Value::List(Arc::new(arms_list))),
                    (Arc::from("default"), default_value.map(|v| (*v).clone()).unwrap_or(Value::Unit)),
                ])),
            };
            }
        }

        // Static evaluation for non-CellRef inputs
        for arm in arms {
            if let Some(bindings) = self.match_pattern(value, &arm.pattern) {
                // Create new runtime with pattern bindings
                let mut match_runtime = BoonDdRuntime {
                    variables: self.variables.clone(),
                    functions: self.functions.clone(),
                    passed_context: self.passed_context.clone(),
                    link_counter: 0,
                    cell_counter: 0,
                    context_path: Vec::new(),
                    last_list_source: None,
                dataflow_config: DataflowConfig::new(),
                    cell_to_collection: HashMap::new(),
                };
                for (name, bound_value) in bindings {
                    match_runtime.variables.insert(name, bound_value);
                }
                return match_runtime.eval_expression(&arm.body.node);
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
                todo!("Complex patterns (Object, List, Map) not yet implemented for reactive WHEN/WHILE")
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
                            let field_value = fields.get(var.name.as_str()).cloned().unwrap_or(Value::Unit);
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
                        let field_value = fields.get(var.name.as_str()).cloned().unwrap_or(Value::Unit);
                        bindings.push((var.name.as_str().to_string(), field_value));
                    }
                    Some(bindings)
                } else {
                    None
                }
            }

            Pattern::List { items } => {
                if let Value::List(list_items) = value {
                    if items.len() != list_items.len() {
                        return None;
                    }
                    let mut bindings = vec![];
                    for (pattern_item, value_item) in items.iter().zip(list_items.iter()) {
                        if let Some(item_bindings) = self.match_pattern(value_item, pattern_item) {
                            bindings.extend(item_bindings);
                        } else {
                            return None;
                        }
                    }
                    Some(bindings)
                } else {
                    None
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
                // Phase 4: DD-native equality comparison
                // Replaces removed WhileRef and ComputedRef patterns with DD collection operators
                match (&a, &b) {
                    // Collection == Collection => DD Equal operator
                    // Used for: all_completed: completed_list_count == list_count
                    // Phase 4: Replaces ComputedRef::Equal with DD-native pattern
                    (Value::Collection(left_handle), Value::Collection(right_handle)) => {
                        self.create_equal(left_handle.id.clone(), right_handle.id.clone())
                    }
                    // Collection == Number => We need to compare collection's value to the number
                    // For now, treat the Collection's snapshot value and compare
                    // TODO: Add a DD operator for comparing collection value to constant
                    (Value::Collection(handle), Value::Number(_)) |
                    (Value::Number(_), Value::Collection(handle)) => {
                        #[cfg(debug_assertions)]
                        zoon::println!("[DD_EVAL] Reactive equality: Collection({:?}) == Number", handle.id);
                        // For Collection == Number, we'd need a CompareToConstant operator
                        // For now, fall through to regular comparison
                        Value::Bool(a == b)
                    }
                    _ => {
                        #[cfg(debug_assertions)]
                        zoon::println!("[DD_EVAL] Comparing {:?} == {:?} => {:?}", a, b, a == b);
                        Value::Bool(a == b)
                    }
                }
            }
            Comparator::NotEqual { operand_a, operand_b } => {
                let a = self.eval_expression(&operand_a.node);
                let b = self.eval_expression(&operand_b.node);
                Value::Bool(a != b)
            }
            Comparator::Less { operand_a, operand_b } => {
                let a = self.eval_expression(&operand_a.node);
                let b = self.eval_expression(&operand_b.node);
                Value::Bool(a < b)
            }
            Comparator::Greater { operand_a, operand_b } => {
                let a = self.eval_expression(&operand_a.node);
                let b = self.eval_expression(&operand_b.node);
                match (&a, &b) {
                    // Collection > 0 => DD GreaterThanZero operator
                    // Used for: show_clear_completed: completed_list_count > 0
                    // Phase 4: Replaces ComputedRef::GreaterThanZero with DD-native pattern
                    (Value::Collection(handle), Value::Number(n)) if n.0 == 0.0 => {
                        self.create_greater_than_zero(handle.id.clone())
                    }
                    _ => Value::Bool(a > b),
                }
            }
            Comparator::LessOrEqual { operand_a, operand_b } => {
                let a = self.eval_expression(&operand_a.node);
                let b = self.eval_expression(&operand_b.node);
                Value::Bool(a <= b)
            }
            Comparator::GreaterOrEqual { operand_a, operand_b } => {
                let a = self.eval_expression(&operand_a.node);
                let b = self.eval_expression(&operand_b.node);
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
                    _ => Value::Unit,
                }
            }
            ArithmeticOperator::Subtract { operand_a, operand_b } => {
                let a = self.eval_expression(&operand_a.node);
                let b = self.eval_expression(&operand_b.node);
                zoon::println!("[DD_EVAL] Subtract: a={:?}, b={:?}", a, b);
                match (&a, &b) {
                    (Value::Number(x), Value::Number(y)) => Value::float(x.0 - y.0),
                    // Collection - Collection => DD Subtract operator
                    // Used for: active_list_count: list_count - completed_list_count
                    // Phase 4: Replaces ComputedRef::Subtract with DD-native pattern
                    (Value::Collection(left_handle), Value::Collection(right_handle)) => {
                        zoon::println!("[DD_EVAL] Creating DD Subtract: {:?} - {:?}", left_handle.id, right_handle.id);
                        self.create_subtract(left_handle.id.clone(), right_handle.id.clone())
                    }
                    _ => {
                        zoon::println!("[DD_EVAL] Subtract: returning Unit (no match)");
                        Value::Unit
                    }
                }
            }
            ArithmeticOperator::Multiply { operand_a, operand_b } => {
                let a = self.eval_expression(&operand_a.node);
                let b = self.eval_expression(&operand_b.node);
                match (&a, &b) {
                    (Value::Number(x), Value::Number(y)) => Value::float(x.0 * y.0),
                    _ => Value::Unit,
                }
            }
            ArithmeticOperator::Divide { operand_a, operand_b } => {
                let a = self.eval_expression(&operand_a.node);
                let b = self.eval_expression(&operand_b.node);
                match (&a, &b) {
                    (Value::Number(x), Value::Number(y)) if y.0 != 0.0 => Value::float(x.0 / y.0),
                    _ => Value::Unit,
                }
            }
            ArithmeticOperator::Negate { operand } => {
                let a = self.eval_expression(&operand.node);
                match &a {
                    Value::Number(x) => Value::float(-x.0),
                    _ => Value::Unit,
                }
            }
        }
    }

    /// Evaluate an alias (variable reference with optional field path).
    fn eval_alias(&mut self, alias: &Alias) -> Value {
        match alias {
            Alias::WithoutPassed { parts, .. } => {
                if parts.is_empty() {
                    return Value::Unit;
                }

                // First part is the variable name
                let mut current = self
                    .variables
                    .get(parts[0].as_str())
                    .cloned()
                    .unwrap_or(Value::Unit);

                // Track the last field name that contained a list (for source_hold tracking)
                let mut list_source_name: Option<String> = None;
                if matches!(current, Value::List(_)) {
                    list_source_name = Some(parts[0].to_string());
                }

                // Rest are field accesses
                for field in parts.iter().skip(1) {
                    // Handle Placeholder specially - create Tagged placeholder for field access
                    // Pure DD: Use Tagged value instead of symbolic PlaceholderField
                    current = match &current {
                        Value::Placeholder => {
                            Value::Tagged {
                                tag: Arc::from("__placeholder_field__"),
                                fields: Arc::new(BTreeMap::from([(
                                    Arc::from("path"),
                                    Value::List(Arc::new(vec![Value::text(field.as_str())])),
                                )])),
                            }
                        }
                        Value::Tagged { tag, fields } if tag.as_ref() == "__placeholder_field__" => {
                            // Extend the existing path
                            let existing_path = fields.get("path")
                                .and_then(|v| match v {
                                    Value::List(items) => Some(items.as_ref().clone()),
                                    _ => None,
                                })
                                .unwrap_or_default();
                            let mut new_path = existing_path;
                            new_path.push(Value::text(field.as_str()));
                            Value::Tagged {
                                tag: Arc::from("__placeholder_field__"),
                                fields: Arc::new(BTreeMap::from([(
                                    Arc::from("path"),
                                    Value::List(Arc::new(new_path)),
                                )])),
                            }
                        }
                        // Handle LinkRef.event - create synthetic event object with all event types
                        Value::LinkRef(link_id) if field.as_str() == "event" => {
                            let link_ref = Value::LinkRef(link_id.clone());
                            Value::object([
                                ("press", link_ref.clone()),
                                ("click", link_ref.clone()),
                                ("blur", link_ref.clone()),
                                ("key_down", link_ref.clone()),
                                ("double_click", link_ref.clone()),
                                ("change", link_ref.clone()),
                            ])
                        }
                        _ => current.get(field.as_str()).cloned().unwrap_or(Value::Unit),
                    };
                    // Update list_source_name if this field is a List
                    if matches!(current, Value::List(_)) {
                        list_source_name = Some(field.to_string());
                    }
                }

                // If the result is a List, track its source name
                if matches!(current, Value::List(_)) {
                    if let Some(name) = list_source_name {
                        self.last_list_source = Some(name);
                    }
                }

                current
            }
            Alias::WithPassed { extra_parts } => {
                // PASSED value - access the passed_context and navigate through fields
                let mut current = self.passed_context.clone().unwrap_or(Value::Unit);

                // Track list source for PASSED context too
                let mut list_source_name: Option<String> = None;

                // Navigate through extra_parts (field accesses after PASSED)
                for field in extra_parts {
                    // Handle Placeholder specially - create Tagged placeholder for field access
                    // Pure DD: Use Tagged value instead of symbolic PlaceholderField
                    current = match &current {
                        Value::Placeholder => {
                            Value::Tagged {
                                tag: Arc::from("__placeholder_field__"),
                                fields: Arc::new(BTreeMap::from([(
                                    Arc::from("path"),
                                    Value::List(Arc::new(vec![Value::text(field.as_str())])),
                                )])),
                            }
                        }
                        Value::Tagged { tag, fields } if tag.as_ref() == "__placeholder_field__" => {
                            // Extend the existing path
                            let existing_path = fields.get("path")
                                .and_then(|v| match v {
                                    Value::List(items) => Some(items.as_ref().clone()),
                                    _ => None,
                                })
                                .unwrap_or_default();
                            let mut new_path = existing_path;
                            new_path.push(Value::text(field.as_str()));
                            Value::Tagged {
                                tag: Arc::from("__placeholder_field__"),
                                fields: Arc::new(BTreeMap::from([(
                                    Arc::from("path"),
                                    Value::List(Arc::new(new_path)),
                                )])),
                            }
                        }
                        // Handle LinkRef.event - create synthetic event object with all event types
                        Value::LinkRef(link_id) if field.as_str() == "event" => {
                            let link_ref = Value::LinkRef(link_id.clone());
                            Value::object([
                                ("press", link_ref.clone()),
                                ("click", link_ref.clone()),
                                ("blur", link_ref.clone()),
                                ("key_down", link_ref.clone()),
                                ("double_click", link_ref.clone()),
                                ("change", link_ref.clone()),
                            ])
                        }
                        _ => current.get(field.as_str()).cloned().unwrap_or(Value::Unit),
                    };
                    if matches!(current, Value::List(_)) {
                        list_source_name = Some(field.to_string());
                    }
                }

                // If the result is a List, check if there's a reactive HOLD for this field.
                // The interpreter creates HOLDs for reactive list operations (List/append, List/clear).
                // If such a HOLD exists, return CellRef so List/map creates MappedListRef.
                if matches!(current, Value::List(_) | Value::Unit) {
                    if let Some(ref name) = list_source_name {
                        // Check if a HOLD with this name exists
                        if super::super::io::get_cell_value(name).is_some() {
                            zoon::println!("[DD_EVAL] PASSED.{} is reactive list - returning CellRef", name);
                            return Value::CellRef(CellId::new(name.as_str()));
                        }
                        self.last_list_source = Some(name.clone());
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
    fn replace_link_ref_in_value(&self, value: &Value, target_id: &LinkId) -> Value {
        use std::collections::BTreeMap;

        match value {
            // Replace any LinkRef with the target
            Value::LinkRef(_) => Value::LinkRef(target_id.clone()),

            // Recursively process objects
            Value::Object(fields) => {
                let new_fields: BTreeMap<Arc<str>, Value> = fields
                    .iter()
                    .map(|(k, v)| (k.clone(), self.replace_link_ref_in_value(v, target_id)))
                    .collect();
                Value::Object(Arc::new(new_fields))
            }

            // Recursively process tagged values
            Value::Tagged { tag, fields } => {
                let new_fields: BTreeMap<Arc<str>, Value> = fields
                    .iter()
                    .map(|(k, v)| (k.clone(), self.replace_link_ref_in_value(v, target_id)))
                    .collect();
                Value::Tagged {
                    tag: tag.clone(),
                    fields: Arc::new(new_fields),
                }
            }

            // Recursively process lists
            Value::List(items) => {
                let new_items: Vec<Value> = items
                    .iter()
                    .map(|item| self.replace_link_ref_in_value(item, target_id))
                    .collect();
                Value::List(Arc::new(new_items))
            }

            // Other values pass through unchanged
            _ => value.clone(),
        }
    }

    /// Replace any LinkRef in a value with a placeholder field Tagged value.
    ///
    /// This is used by `|> LINK { alias }` during template evaluation when the alias
    /// resolves to a placeholder field (deferred field access). The placeholder
    /// will be resolved to a real LinkRef during template cloning/substitution.
    ///
    /// Pure DD: Uses Tagged value with "__placeholder_field__" tag instead of symbolic ref.
    fn replace_link_ref_with_placeholder(&self, value: &Value, path: &[Arc<str>]) -> Value {
        use std::collections::BTreeMap;

        match value {
            // Replace any LinkRef with the placeholder Tagged value
            Value::LinkRef(_) => {
                let path_list: Vec<Value> = path.iter().map(|s| Value::text(s.as_ref())).collect();
                Value::Tagged {
                    tag: Arc::from("__placeholder_field__"),
                    fields: Arc::new(BTreeMap::from([(
                        Arc::from("path"),
                        Value::List(Arc::new(path_list)),
                    )])),
                }
            }

            // Recursively process objects
            Value::Object(fields) => {
                let new_fields: BTreeMap<Arc<str>, Value> = fields
                    .iter()
                    .map(|(k, v)| (k.clone(), self.replace_link_ref_with_placeholder(v, path)))
                    .collect();
                Value::Object(Arc::new(new_fields))
            }

            // Recursively process tagged values
            Value::Tagged { tag, fields } => {
                let new_fields: BTreeMap<Arc<str>, Value> = fields
                    .iter()
                    .map(|(k, v)| (k.clone(), self.replace_link_ref_with_placeholder(v, path)))
                    .collect();
                Value::Tagged {
                    tag: tag.clone(),
                    fields: Arc::new(new_fields),
                }
            }

            // Recursively process lists
            Value::List(items) => {
                let new_items: Vec<Value> = items
                    .iter()
                    .map(|item| self.replace_link_ref_with_placeholder(item, path))
                    .collect();
                Value::List(Arc::new(new_items))
            }

            // Other values pass through unchanged
            _ => value.clone(),
        }
    }

    /// Compare two Values for equality.
    /// Handles Tagged comparison by comparing only the tag name.
    fn values_equal(&self, a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Tagged { tag: tag_a, .. }, Value::Tagged { tag: tag_b, .. }) => {
                tag_a.as_ref() == tag_b.as_ref()
            }
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Number(a), Value::Number(b)) => a == b,
            (Value::Text(a), Value::Text(b)) => a.as_ref() == b.as_ref(),
            (Value::Unit, Value::Unit) => true,
            _ => false,
        }
    }
}

impl Default for BoonDdRuntime {
    fn default() -> Self {
        Self::new()
    }
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
}
