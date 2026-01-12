//! DD-based evaluator for Boon.
//!
//! This module evaluates Boon AST using simple value types.
//! This is phase 1: static evaluation without reactive features.
//! Phase 2 will add DD-based reactive evaluation.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use super::dd_value::{DdValue, ComputedType};
use super::io::{init_hold_state, add_router_mapping};

// Global counter for generating unique HOLD IDs across all runtime instances
static GLOBAL_HOLD_COUNTER: AtomicU32 = AtomicU32::new(0);

// Global counter for generating unique LINK IDs across all runtime instances
static GLOBAL_LINK_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Reset the global counters (call at start of each evaluation)
pub fn reset_hold_counter() {
    GLOBAL_HOLD_COUNTER.store(0, Ordering::SeqCst);
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
    variables: HashMap<String, DdValue>,
    /// Function definitions
    functions: HashMap<String, FunctionDef>,
    /// PASSED context for function calls
    passed_context: Option<DdValue>,
    /// Counter for generating unique LINK IDs
    link_counter: u32,
    /// Counter for generating unique HOLD IDs
    hold_counter: u32,
    /// Current context path for LINK naming (e.g., "increment_button.event.press")
    context_path: Vec<String>,
}

impl BoonDdRuntime {
    /// Create a new DD runtime.
    pub fn new() -> Self {
        Self {
            variables: HashMap::new(),
            functions: HashMap::new(),
            passed_context: None,
            link_counter: 0,
            hold_counter: 0,
            context_path: Vec::new(),
        }
    }

    /// Generate a unique HOLD ID using global counter.
    fn generate_hold_id(&mut self) -> String {
        let id = GLOBAL_HOLD_COUNTER.fetch_add(1, Ordering::SeqCst);
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
    pub fn get_variable(&self, name: &str) -> Option<&DdValue> {
        self.variables.get(name)
    }

    /// Get all variables.
    pub fn get_all_variables(&self) -> &HashMap<String, DdValue> {
        &self.variables
    }

    /// Get the document output (the root rendering output).
    pub fn get_document(&self) -> Option<&DdValue> {
        self.get_variable("document")
    }

    /// Inject a variable value before evaluation.
    /// This allows external state (from ReactiveContext) to override
    /// AST-defined variables.
    pub fn inject_variable(&mut self, name: impl Into<String>, value: DdValue) {
        self.variables.insert(name.into(), value);
    }

    /// Inject multiple variables at once.
    pub fn inject_variables(&mut self, vars: impl IntoIterator<Item = (String, DdValue)>) {
        for (name, value) in vars {
            self.variables.insert(name, value);
        }
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
    fn eval_expression(&mut self, expr: &Expression) -> DdValue {
        match expr {
            // Literals
            Expression::Literal(lit) => self.eval_literal(lit),

            // Alias: variable reference with optional field path
            Expression::Alias(alias) => self.eval_alias(alias),

            // Object literal: [field: value, ...]
            Expression::Object(obj) => self.eval_object(obj),

            // List literal: LIST { a, b, c }
            Expression::List { items } => {
                let values: Vec<DdValue> = items
                    .iter()
                    .map(|spanned| self.eval_expression(&spanned.node))
                    .collect();
                DdValue::list(values)
            }

            // Text literal: TEXT { ... }
            Expression::TextLiteral { parts } => self.eval_text_literal(parts),

            // Function call
            Expression::FunctionCall { path, arguments } => {
                self.eval_function_call(path, arguments)
            }

            // Pipe: a |> b
            Expression::Pipe { from, to } => {
                // Check for LATEST { ... THEN ... } |> Math/sum() pattern
                // This is the event-driven accumulator pattern
                if self.is_latest_sum_pattern(&from.node, &to.node) {
                    // Get initial value from LATEST (first non-unit input)
                    let initial = self.get_latest_initial(&from.node);

                    // Generate unique HOLD ID for this pattern
                    let hold_id = self.generate_hold_id();
                    init_hold_state(&hold_id, initial.clone());

                    zoon::println!("[DD_EVAL] LATEST+sum pattern detected: {} with initial {:?}", hold_id, initial);

                    // Return HoldRef - bridge will render reactively
                    return DdValue::HoldRef(Arc::from(hold_id.as_str()));
                }

                // Check for Timer/interval() |> THEN { ... } |> Math/sum() pattern
                // This is the timer-driven accumulator pattern
                if self.is_timer_sum_pattern(&from.node, &to.node) {
                    // Extract timer info from the pattern
                    if let Some((timer_id, interval_ms)) = self.extract_timer_info(&from.node) {
                        let hold_id = "timer_counter";
                        // NOTE: Do NOT call init_hold_state here!
                        // The test expects empty output until the first timer fires.
                        // The interpreter will handle initialization via DataflowConfig.

                        zoon::println!("[DD_EVAL] Timer+sum pattern detected: {} with timer {} @ {}ms", hold_id, timer_id, interval_ms);

                        // Return TimerRef so interpreter can set up the timer
                        return DdValue::TimerRef {
                            id: Arc::from(hold_id),
                            interval_ms
                        };
                    }
                }

                // Check for LATEST { ... THEN ... } |> Router/go_to() pattern
                // This is the navigation pattern - extract link→route mappings
                if self.is_latest_router_pattern(&from.node, &to.node) {
                    self.extract_router_mappings(&from.node);
                    zoon::println!("[DD_EVAL] Router navigation pattern detected");
                    // Return Unit - router actions happen on link fire
                    return DdValue::Unit;
                }

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
                    hold_counter: 0,
                    context_path: Vec::new(),
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

            // LATEST { a, b, c } - for static eval, return first non-unit
            Expression::Latest { inputs } => {
                for input in inputs {
                    let val = self.eval_expression(&input.node);
                    if val != DdValue::Unit {
                        return val;
                    }
                }
                DdValue::Unit
            }

            // HOLD - for static eval, return unit (needs pipe context)
            Expression::Hold { .. } => DdValue::Unit,

            // THEN - for static eval, return unit (needs event)
            Expression::Then { .. } => DdValue::Unit,

            // WHEN/WHILE - try to match patterns for static values
            Expression::When { arms } | Expression::While { arms } => {
                // For static eval, need piped value - return Unit (handled in eval_pipe)
                DdValue::Unit
            }

            // LINK - create a LinkRef with a unique ID for reactive wiring
            Expression::Link => {
                let link_id = self.generate_link_id();
                DdValue::link_ref(link_id)
            }

            // Skip
            Expression::Skip => DdValue::Unit,

            // Tagged object
            Expression::TaggedObject { tag, object } => {
                let fields = self.eval_object(object);
                if let DdValue::Object(map) = fields {
                    DdValue::Tagged {
                        tag: Arc::from(tag.as_str()),
                        fields: map,
                    }
                } else {
                    DdValue::Unit
                }
            }

            // Variable definition (shouldn't appear here normally)
            Expression::Variable(var) => self.eval_expression(&var.value.node),

            // Field access: .field.path
            Expression::FieldAccess { path } => {
                // This should only appear in pipe context
                DdValue::Unit
            }

            // Fallback for unhandled expressions
            _ => DdValue::Unit,
        }
    }

    /// Evaluate a literal.
    fn eval_literal(&mut self, lit: &Literal) -> DdValue {
        match lit {
            Literal::Number(n) => DdValue::float(*n),
            Literal::Text(s) => DdValue::text(s.as_str()),
            Literal::Tag(s) => DdValue::Tagged {
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
    fn eval_object(&mut self, obj: &Object) -> DdValue {
        // Create a scoped runtime with access to current variables
        let mut scoped_runtime = BoonDdRuntime {
            variables: self.variables.clone(),
            functions: self.functions.clone(),
            passed_context: self.passed_context.clone(),
            link_counter: 0,
            hold_counter: 0,
            context_path: Vec::new(),
        };

        let mut map = BTreeMap::new();
        for var in &obj.variables {
            let name_str = var.node.name.as_str();
            let name = Arc::from(name_str);
            let value = scoped_runtime.eval_expression(&var.node.value.node);

            // Add to both the result map and the scoped runtime
            // so later variables can reference earlier ones
            map.insert(name, value.clone());
            scoped_runtime.variables.insert(name_str.to_string(), value);
        }
        DdValue::Object(Arc::new(map))
    }

    /// Evaluate a text literal with interpolation.
    fn eval_text_literal(&mut self, parts: &[TextPart]) -> DdValue {
        let mut result = String::new();
        for part in parts {
            match part {
                TextPart::Text(s) => result.push_str(s.as_str()),
                TextPart::Interpolation { var, .. } => {
                    if let Some(value) = self.variables.get(var.as_str()) {
                        result.push_str(&value.to_display_string());
                    }
                }
            }
        }
        DdValue::text(result)
    }

    /// Evaluate a function call.
    fn eval_function_call(
        &mut self,
        path: &[crate::parser::StrSlice],
        arguments: &[Spanned<crate::parser::static_expression::Argument>],
    ) -> DdValue {
        // Build argument map
        let args: HashMap<&str, DdValue> = arguments
            .iter()
            .filter_map(|arg| {
                let name = arg.node.name.as_str();
                let value = arg.node.value.as_ref().map(|v| self.eval_expression(&v.node))?;
                Some((name, value))
            })
            .collect();

        // Convert path to namespace/name
        let full_path: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
        let (namespace, name) = if full_path.len() >= 2 {
            (Some(full_path[0]), full_path[1])
        } else if full_path.len() == 1 {
            (None, full_path[0])
        } else {
            return DdValue::Unit;
        };

        match (namespace, name) {
            // Document/new(root: value)
            (Some("Document"), "new") => {
                args.get("root").cloned().unwrap_or(DdValue::Unit)
            }

            // Math/sum() - returns 0 for static
            (Some("Math"), "sum") => DdValue::int(0),

            // Timer/interval - returns unit in static context
            (Some("Timer"), "interval") => DdValue::Unit,

            // Stream/pulses - returns unit in static context
            (Some("Stream"), "pulses") => DdValue::Unit,

            // Element functions
            (Some("Element"), func) => self.eval_element_function(func, &args),

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
            _ => DdValue::Unit,
        }
    }

    /// Evaluate a user-defined function call.
    fn eval_user_function(&mut self, name: &str, args: &HashMap<&str, DdValue>) -> DdValue {
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
                hold_counter: 0,
                context_path: Vec::new(),
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
            DdValue::Unit
        }
    }

    /// Evaluate a user-defined function with a piped value.
    /// The piped value becomes the first parameter of the function.
    fn eval_user_function_with_piped(
        &self,
        name: &str,
        piped: &DdValue,
        args: &HashMap<&str, DdValue>,
    ) -> DdValue {
        if let Some(func_def) = self.functions.get(name) {
            // Check for PASS: argument to set passed_context
            let passed_context = args.get("PASS").cloned().or_else(|| self.passed_context.clone());

            // Create a new runtime with the function arguments as variables
            let mut func_runtime = BoonDdRuntime {
                variables: self.variables.clone(),
                functions: self.functions.clone(),
                passed_context,
                link_counter: 0,
                hold_counter: 0,
                context_path: Vec::new(),
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
            DdValue::Unit
        }
    }

    /// Evaluate an Element function.
    fn eval_element_function(&mut self, name: &str, args: &HashMap<&str, DdValue>) -> DdValue {
        let mut fields: Vec<(&str, DdValue)> = vec![("_element_type", DdValue::text(name))];
        for (k, v) in args {
            fields.push((k, v.clone()));
        }
        DdValue::tagged("Element", fields.into_iter())
    }

    /// Evaluate a List function.
    fn eval_list_function(&mut self, name: &str, args: &HashMap<&str, DdValue>) -> DdValue {
        match name {
            "count" => {
                if let Some(DdValue::List(items)) = args.values().next() {
                    DdValue::int(items.len() as i64)
                } else {
                    DdValue::int(0)
                }
            }
            "is_empty" => {
                if let Some(DdValue::List(items)) = args.values().next() {
                    DdValue::Bool(items.is_empty())
                } else {
                    DdValue::Bool(true)
                }
            }
            _ => DdValue::Unit,
        }
    }

    /// Evaluate a Router function.
    fn eval_router_function(&mut self, name: &str, _args: &HashMap<&str, DdValue>) -> DdValue {
        match name {
            // Router/route() - returns a HoldRef to the current route
            // The actual route value is stored in HOLD_STATES["current_route"]
            // and updated by navigation events via set_filter_from_route()
            "route" => {
                // Initialize the current_route HOLD with the browser's current path
                #[cfg(target_arch = "wasm32")]
                {
                    super::io::init_current_route();
                    let path = super::io::get_current_route();
                    // Store in HOLD_STATES so bridge can render it reactively
                    super::io::update_hold_state_no_persist("current_route", DdValue::text(path));
                }
                #[cfg(not(target_arch = "wasm32"))]
                {
                    super::io::update_hold_state_no_persist("current_route", DdValue::text("/"));
                }
                // Return HoldRef so WHEN can observe route changes reactively
                DdValue::HoldRef(std::sync::Arc::from("current_route"))
            }
            // Router/go_to(route) - navigation (no-op in static context)
            "go_to" => DdValue::Unit,
            _ => DdValue::Unit,
        }
    }

    /// Evaluate a Text function.
    fn eval_text_function(&mut self, name: &str, args: &HashMap<&str, DdValue>) -> DdValue {
        match name {
            // Text/trim() - trim whitespace
            "trim" => {
                if let Some(DdValue::Text(s)) = args.values().next() {
                    DdValue::text(s.trim())
                } else {
                    DdValue::text("")
                }
            }
            // Text/is_not_empty() -> Bool
            "is_not_empty" => {
                if let Some(DdValue::Text(s)) = args.values().next() {
                    DdValue::Bool(!s.is_empty())
                } else {
                    DdValue::Bool(false)
                }
            }
            // Text/is_empty() -> Bool
            "is_empty" => {
                if let Some(DdValue::Text(s)) = args.values().next() {
                    DdValue::Bool(s.is_empty())
                } else {
                    DdValue::Bool(true)
                }
            }
            // Text/empty() -> ""
            "empty" => DdValue::text(""),
            _ => DdValue::Unit,
        }
    }

    /// Evaluate a pipe expression.
    fn eval_pipe(&mut self, from: &DdValue, to: &Expression) -> DdValue {
        match to {
            // Pipe to function call
            Expression::FunctionCall { path, arguments } => {
                let full_path: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                let (namespace, name) = if full_path.len() >= 2 {
                    (Some(full_path[0]), full_path[1])
                } else if full_path.len() == 1 {
                    (None, full_path[0])
                } else {
                    return DdValue::Unit;
                };

                // Build args
                let args: HashMap<&str, DdValue> = arguments
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
                        args.get("root").cloned().unwrap_or(DdValue::Unit)
                    }
                    (Some("Math"), "sum") => from.clone(),
                    (Some("Timer"), "interval") => {
                        // Duration |> Timer/interval() - returns TimerRef
                        // Extract interval from Duration[seconds: n] or Duration[millis: n]
                        let interval_ms = match from {
                            DdValue::Tagged { tag, fields } if tag.as_ref() == "Duration" => {
                                if let Some(DdValue::Number(secs)) = fields.get("seconds") {
                                    (secs.0 * 1000.0) as u64
                                } else if let Some(DdValue::Number(ms)) = fields.get("millis") {
                                    ms.0 as u64
                                } else {
                                    1000 // Default 1 second
                                }
                            }
                            DdValue::Number(ms) => ms.0 as u64,
                            _ => 1000, // Default 1 second
                        };
                        let timer_id = format!("timer_{}", interval_ms);
                        zoon::println!("[DD_EVAL] Timer/interval: {}ms -> {}", interval_ms, timer_id);
                        DdValue::timer_ref(timer_id, interval_ms)
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
                    (Some("List"), "remove") => {
                        // from |> List/remove(item, on: ...) - pass through for static eval
                        // (removal depends on events which don't fire in static eval)
                        from.clone()
                    }
                    (Some("List"), "retain") => {
                        // from |> List/retain(item, if: ...) - filter items based on predicate
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

                        // Handle HoldRef input - create FilteredListRef for reactive evaluation
                        if let DdValue::HoldRef(hold_id) = from {
                            if let (Some(binding), Some(pred_expr)) = (binding_name, predicate_expr) {
                                // Try to extract field access pattern: `item.field`
                                if let Some((field_name, filter_value)) =
                                    self.extract_field_filter(binding, &pred_expr.node)
                                {
                                    return DdValue::FilteredListRef {
                                        source_hold: hold_id.clone(),
                                        filter_field: Arc::from(field_name),
                                        filter_value: Box::new(filter_value),
                                    };
                                }
                            }
                            // Fallback: return HoldRef unchanged
                            return from.clone();
                        }

                        // Handle concrete List input - check if filter references HoldRef fields
                        if let (Some(binding), Some(pred_expr), DdValue::List(items)) =
                            (binding_name, predicate_expr, from)
                        {
                            // Try to extract field access pattern: `item.field`
                            if let Some((field_name, filter_value)) =
                                self.extract_field_filter(binding, &pred_expr.node)
                            {
                                // Check if the field contains HoldRefs in any item
                                let hold_ids: Vec<Arc<str>> = items
                                    .iter()
                                    .filter_map(|item| {
                                        if let DdValue::Object(obj) = item {
                                            if let Some(DdValue::HoldRef(hold_id)) = obj.get(field_name.as_str()) {
                                                return Some(hold_id.clone());
                                            }
                                        }
                                        None
                                    })
                                    .collect();

                                // If all items have HoldRef fields, create reactive filtered list
                                if hold_ids.len() == items.len() && !hold_ids.is_empty() {
                                    return DdValue::ReactiveFilteredList {
                                        items: items.clone(),
                                        filter_field: Arc::from(field_name),
                                        filter_value: Box::new(filter_value),
                                        hold_ids: Arc::new(hold_ids),
                                    };
                                }
                            }

                            // Fallback: filter immediately (for non-reactive cases)
                            let filtered: Vec<DdValue> = items
                                .iter()
                                .filter(|item| {
                                    // Create scoped runtime with the item bound
                                    let mut scoped = BoonDdRuntime {
                                        variables: self.variables.clone(),
                                        functions: self.functions.clone(),
                                        passed_context: self.passed_context.clone(),
                                        link_counter: 0,
                                        hold_counter: 0,
                                        context_path: Vec::new(),
                                    };
                                    scoped.variables.insert(binding.to_string(), (*item).clone());

                                    // Evaluate the predicate
                                    let result = scoped.eval_expression(&pred_expr.node);
                                    result.is_truthy()
                                })
                                .cloned()
                                .collect();
                            DdValue::list(filtered)
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

                        if let (Some(binding), Some(new_expr), DdValue::List(items)) =
                            (binding_name, transform_expr, from)
                        {
                            let mapped: Vec<DdValue> = items
                                .iter()
                                .map(|item| {
                                    // Create scoped runtime with the item bound
                                    let mut scoped = BoonDdRuntime {
                                        variables: self.variables.clone(),
                                        functions: self.functions.clone(),
                                        passed_context: self.passed_context.clone(),
                                        link_counter: 0,
                                        hold_counter: 0,
                                        context_path: Vec::new(),
                                    };
                                    scoped.variables.insert(binding.to_string(), (*item).clone());

                                    // Evaluate the transform expression
                                    scoped.eval_expression(&new_expr.node)
                                })
                                .collect();
                            DdValue::list(mapped)
                        } else {
                            from.clone()
                        }
                    }
                    (Some("List"), "count") => {
                        // from |> List/count() - count items
                        match from {
                            DdValue::List(items) => DdValue::int(items.len() as i64),
                            // HoldRef |> List/count() -> ComputedRef::ListCount
                            DdValue::HoldRef(hold_id) => DdValue::computed_ref(
                                ComputedType::ListCount,
                                hold_id.clone(),
                            ),
                            // FilteredListRef |> List/count() -> ComputedRef::ListCountWhere
                            DdValue::FilteredListRef {
                                source_hold,
                                filter_field,
                                filter_value,
                            } => DdValue::computed_ref(
                                ComputedType::ListCountWhere {
                                    field: filter_field.clone(),
                                    value: filter_value.clone(),
                                },
                                source_hold.clone(),
                            ),
                            // ReactiveFilteredList |> List/count() -> ComputedRef::ReactiveListCountWhere
                            DdValue::ReactiveFilteredList {
                                items,
                                filter_field,
                                filter_value,
                                hold_ids,
                            } => {
                                // Use first hold_id as source (for bridge reactivity)
                                // The actual counting is done by ReactiveListCountWhere which uses all hold_ids
                                let source = hold_ids.first().cloned().unwrap_or_else(|| Arc::from(""));
                                DdValue::ComputedRef {
                                    computation: ComputedType::ReactiveListCountWhere {
                                        items: items.clone(),
                                        field: filter_field.clone(),
                                        value: filter_value.clone(),
                                        hold_ids: hold_ids.clone(),
                                    },
                                    source_hold: source,
                                }
                            }
                            _ => DdValue::int(0),
                        }
                    }
                    (Some("List"), "is_empty") => {
                        // from |> List/is_empty()
                        if let DdValue::List(items) = from {
                            DdValue::Bool(items.is_empty())
                        } else {
                            DdValue::Bool(true)
                        }
                    }
                    // Bool operations
                    (Some("Bool"), "or") => {
                        // from |> Bool/or(that: other_bool)
                        let from_bool = match from {
                            DdValue::Bool(b) => *b,
                            DdValue::Tagged { tag, .. } => tag.as_ref() == "True",
                            _ => from.is_truthy(),
                        };
                        let that_bool = args.get("that").map(|v| match v {
                            DdValue::Bool(b) => *b,
                            DdValue::Tagged { tag, .. } => tag.as_ref() == "True",
                            _ => v.is_truthy(),
                        }).unwrap_or(false);
                        DdValue::Bool(from_bool || that_bool)
                    }
                    (Some("Bool"), "and") => {
                        // from |> Bool/and(that: other_bool)
                        let from_bool = match from {
                            DdValue::Bool(b) => *b,
                            DdValue::Tagged { tag, .. } => tag.as_ref() == "True",
                            _ => from.is_truthy(),
                        };
                        let that_bool = args.get("that").map(|v| match v {
                            DdValue::Bool(b) => *b,
                            DdValue::Tagged { tag, .. } => tag.as_ref() == "True",
                            _ => v.is_truthy(),
                        }).unwrap_or(true);
                        DdValue::Bool(from_bool && that_bool)
                    }
                    (Some("Bool"), "not") => {
                        // from |> Bool/not()
                        let from_bool = match from {
                            DdValue::Bool(b) => *b,
                            DdValue::Tagged { tag, .. } => tag.as_ref() == "True",
                            _ => from.is_truthy(),
                        };
                        DdValue::Bool(!from_bool)
                    }
                    // Text functions (piped)
                    (Some("Text"), "trim") => {
                        // from |> Text/trim()
                        if let DdValue::Text(s) = from {
                            DdValue::text(s.trim())
                        } else {
                            from.clone()
                        }
                    }
                    (Some("Text"), "is_not_empty") => {
                        // from |> Text/is_not_empty()
                        if let DdValue::Text(s) = from {
                            DdValue::Bool(!s.is_empty())
                        } else {
                            DdValue::Bool(false)
                        }
                    }
                    (Some("Text"), "is_empty") => {
                        // from |> Text/is_empty()
                        if let DdValue::Text(s) = from {
                            DdValue::Bool(s.is_empty())
                        } else {
                            DdValue::Bool(true)
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
            Expression::Then { .. } => DdValue::Unit,

            // Pipe to WHEN - pattern match and return body
            Expression::When { arms } => self.eval_pattern_match(from, arms),

            // Pipe to WHILE - pattern match and return body (same as WHEN for static)
            Expression::While { arms } => self.eval_pattern_match(from, arms),

            // Pipe to field access
            Expression::FieldAccess { path } => {
                let mut current = from.clone();
                for field in path {
                    current = current.get(field.as_str()).cloned().unwrap_or(DdValue::Unit);
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

                // Replace any LinkRef in the element with the target
                if let DdValue::LinkRef(target_id) = &target_link {
                    self.replace_link_ref_in_value(from, target_id)
                } else {
                    // If alias doesn't resolve to a LinkRef, just pass through unchanged
                    from.clone()
                }
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
    fn eval_hold(&mut self, initial: &DdValue, state_name: &str, body: &Expression) -> DdValue {
        // Try to extract pulse count from body: `count |> Stream/pulses() |> ...`
        let pulse_count = self.extract_pulse_count(body);

        if pulse_count == 0 {
            // Check if body contains a timer trigger (variable that evaluates to TimerRef)
            // e.g., `tick |> THEN { state + 1 }` where tick = Duration |> Timer/interval()
            if let Some(interval_ms) = self.extract_timer_trigger_from_body(body) {
                let hold_id = "timer_counter";
                // NOTE: Do NOT call init_hold_state here - test expects empty until first tick
                zoon::println!("[DD_EVAL] Timer-triggered HOLD detected: {} @ {}ms", hold_id, interval_ms);

                // Return TimerRef so interpreter sets up timer-triggered HOLD
                return DdValue::TimerRef {
                    id: Arc::from(hold_id),
                    interval_ms,
                };
            }

            // Check if body contains a LINK trigger (reactive HOLD)
            // e.g., `button.event.press |> THEN { state + 1 }`
            if self.contains_link_trigger(body) {
                // Generate unique HOLD ID for this HOLD instance
                let hold_id = self.generate_hold_id();

                // Store initial value in HOLD_STATES for reactive rendering
                init_hold_state(&hold_id, initial.clone());

                zoon::println!("[DD_EVAL] LINK-triggered HOLD detected: {} with initial {:?}", hold_id, initial);

                // Return HoldRef - bridge will render reactively
                return DdValue::HoldRef(Arc::from(hold_id.as_str()));
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
                hold_counter: 0,
                context_path: Vec::new(),
            };
            iter_runtime.variables.insert(state_name.to_string(), current_state.clone());

            // Evaluate the THEN body to get next state
            let next_state = iter_runtime.eval_expression(then_body);

            // Skip Unit results (like SKIP in WHEN patterns)
            if next_state != DdValue::Unit {
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
                    if let DdValue::Number(n) = self.eval_expression(&from.node) {
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

    /// Extract field filter pattern from predicate expression.
    /// Handles patterns like `item.completed` where `item` is the binding name.
    /// Returns (field_name, filter_value) if pattern matches.
    fn extract_field_filter(
        &mut self,
        binding: &str,
        predicate: &Expression,
    ) -> Option<(String, DdValue)> {
        match predicate {
            // Pattern: `item.field` - field access is the entire predicate
            // Path is [item, field], meaning: filter where field == true
            Expression::FieldAccess { path } => {
                if path.len() == 2 && path[0].as_ref() == binding {
                    return Some((path[1].to_string(), DdValue::Bool(true)));
                }
                None
            }
            // Pattern: `item.field` as Alias (parser produces this for variable.field)
            Expression::Alias(Alias::WithoutPassed { parts, .. }) => {
                if parts.len() == 2 && parts[0].as_ref() == binding {
                    return Some((parts[1].to_string(), DdValue::Bool(true)));
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
                                return Some((path[1].to_string(), DdValue::Bool(false)));
                            }
                        }
                        // Check for Alias
                        if let Expression::Alias(Alias::WithoutPassed { parts, .. }) = &from.node {
                            if parts.len() == 2 && parts[0].as_ref() == binding {
                                return Some((parts[1].to_string(), DdValue::Bool(false)));
                            }
                        }
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Check if expression contains a LINK trigger (reactive event source).
    /// Used to detect LINK-triggered HOLDs like:
    ///   `button.event.press |> THEN { state + 1 }`
    ///
    /// Detection strategy: Look for the `X |> THEN { ... }` pattern which is
    /// the canonical event-driven reactive pattern in Boon. If the HOLD body
    /// pipes something to THEN, it's event-driven and should return HoldRef.
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
                            DdValue::Tagged { tag, fields } if tag.as_ref() == "Duration" => {
                                if let Some(DdValue::Number(secs)) = fields.get("seconds") {
                                    (secs.0 * 1000.0) as u64
                                } else if let Some(DdValue::Number(ms)) = fields.get("millis") {
                                    ms.0 as u64
                                } else {
                                    1000
                                }
                            }
                            DdValue::Number(ms) => ms.0 as u64,
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

    /// Check if expression is the LATEST + Math/sum pattern.
    /// Pattern: `LATEST { initial, event |> THEN { value } } |> Math/sum()`
    fn is_latest_sum_pattern(&self, from: &Expression, to: &Expression) -> bool {
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

        // Check if `from` is LATEST containing at least one THEN pattern
        match from {
            Expression::Latest { inputs } => {
                // At least one input should be or contain a THEN
                inputs.iter().any(|input| self.contains_then_pattern(&input.node))
            }
            _ => false,
        }
    }

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

    /// Extract the initial value from a LATEST expression.
    /// The initial value is the first input that is NOT a THEN pattern.
    fn get_latest_initial(&mut self, expr: &Expression) -> DdValue {
        match expr {
            Expression::Latest { inputs } => {
                // Find first input that is NOT a THEN pattern (that's the initial value)
                for input in inputs {
                    if !self.contains_then_pattern(&input.node) {
                        return self.eval_expression(&input.node);
                    }
                }
                // Default to 0 if no non-THEN input found
                DdValue::float(0.0)
            }
            _ => DdValue::float(0.0),
        }
    }

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
                    if let DdValue::TimerRef { interval_ms, .. } = from_val {
                        return Some(interval_ms);
                    }
                }
                // Recurse into nested pipes
                self.extract_timer_trigger_from_body(&to.node)
            }
            _ => None,
        }
    }

    /// Check if expression is the LATEST → Router/go_to pattern.
    /// Pattern: `LATEST { ... |> THEN { route } ... } |> Router/go_to()`
    fn is_latest_router_pattern(&self, from: &Expression, to: &Expression) -> bool {
        // Check if `to` is Router/go_to()
        let is_router_goto = match to {
            Expression::FunctionCall { path, .. } => {
                let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                path_strs == ["Router", "go_to"]
            }
            _ => false,
        };

        if !is_router_goto {
            return false;
        }

        // Check if `from` is LATEST containing THEN patterns
        matches!(from, Expression::Latest { .. }) && self.contains_then_pattern(from)
    }

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

                        if let (Some(link_id), DdValue::Text(route_text)) = (link_ref, route) {
                            add_router_mapping(&link_id, route_text.as_ref());
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
                    current = current.get(field).cloned().unwrap_or(DdValue::Unit);
                }

                // Extract the link ID from the LinkRef
                if let DdValue::LinkRef(id) = current {
                    Some(id.to_string())
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Evaluate pattern matching for WHEN/WHILE.
    fn eval_pattern_match(&mut self, value: &DdValue, arms: &[Arm]) -> DdValue {
        // If input is a HoldRef, return a WhileRef for reactive rendering
        if let DdValue::HoldRef(hold_id) = value {
            // Pre-evaluate all arms for the bridge to render reactively
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
                        hold_counter: 0,
                        context_path: Vec::new(),
                    };
                    let body_result = match_runtime.eval_expression(&arm.body.node);
                    default_value = Some(Arc::new(body_result));
                } else if let Some(pv) = pattern_value {
                    // Evaluate the body for this pattern
                    let mut match_runtime = BoonDdRuntime {
                        variables: self.variables.clone(),
                        functions: self.functions.clone(),
                        passed_context: self.passed_context.clone(),
                        link_counter: 0,
                        hold_counter: 0,
                        context_path: Vec::new(),
                    };
                    let body_result = match_runtime.eval_expression(&arm.body.node);
                    evaluated_arms.push((pv, body_result));
                }
            }

            zoon::println!("[DD_EVAL] Created WhileRef for hold {} with {} arms", hold_id, evaluated_arms.len());

            return DdValue::WhileRef {
                hold_id: hold_id.clone(),
                computation: None,  // No computation for direct HoldRef
                arms: Arc::new(evaluated_arms),
                default: default_value,
            };
        }

        // If input is a ComputedRef (boolean computation), create a WhileRef with the computation
        // This handles: completed_todos_count > 0 |> WHILE { True => button, False => NoElement }
        if let DdValue::ComputedRef { computation, source_hold } = value {
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
                        hold_counter: 0,
                        context_path: Vec::new(),
                    };
                    let body_result = match_runtime.eval_expression(&arm.body.node);
                    default_value = Some(Arc::new(body_result));
                } else if let Some(pv) = pattern_value {
                    let mut match_runtime = BoonDdRuntime {
                        variables: self.variables.clone(),
                        functions: self.functions.clone(),
                        passed_context: self.passed_context.clone(),
                        link_counter: 0,
                        hold_counter: 0,
                        context_path: Vec::new(),
                    };
                    let body_result = match_runtime.eval_expression(&arm.body.node);
                    evaluated_arms.push((pv, body_result));
                }
            }

            zoon::println!("[DD_EVAL] Created WhileRef for ComputedRef (source: {}) with {} arms", source_hold, evaluated_arms.len());

            return DdValue::WhileRef {
                hold_id: source_hold.clone(),
                computation: Some(computation.clone()),  // Store the computation for bridge to evaluate
                arms: Arc::new(evaluated_arms),
                default: default_value,
            };
        }

        // If input is a WhileRef, chain the pattern matching
        // This happens when: route |> WHEN { "/" => Home } |> WHILE { Home => page(...) }
        if let DdValue::WhileRef { hold_id, computation: input_computation, arms: input_arms, default: input_default } = value {
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
                            (DdValue::Tagged { tag: a, .. }, DdValue::Tagged { tag: b, .. }) => a == b,
                            (DdValue::Text(a), DdValue::Text(b)) => a == b,
                            _ => intermediate_value == pv,
                        };

                        if matches {
                            // Evaluate the body and map from input pattern to body result
                            let mut match_runtime = BoonDdRuntime {
                                variables: self.variables.clone(),
                                functions: self.functions.clone(),
                                passed_context: self.passed_context.clone(),
                                link_counter: 0,
                                hold_counter: 0,
                                context_path: Vec::new(),
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
                            hold_counter: 0,
                            context_path: Vec::new(),
                        };
                        let body_result = match_runtime.eval_expression(&arm.body.node);
                        default_value = Some(Arc::new(body_result));
                        break;
                    }

                    let pattern_value = self.pattern_to_value(&arm.pattern);
                    if let Some(pv) = &pattern_value {
                        let matches = match (input_def.as_ref(), pv) {
                            (DdValue::Tagged { tag: a, .. }, DdValue::Tagged { tag: b, .. }) => a == b,
                            _ => input_def.as_ref() == pv,
                        };
                        if matches {
                            let mut match_runtime = BoonDdRuntime {
                                variables: self.variables.clone(),
                                functions: self.functions.clone(),
                                passed_context: self.passed_context.clone(),
                                link_counter: 0,
                                hold_counter: 0,
                                context_path: Vec::new(),
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
                            hold_counter: 0,
                            context_path: Vec::new(),
                        };
                        let body_result = match_runtime.eval_expression(&arm.body.node);
                        default_value = Some(Arc::new(body_result));
                        break;
                    }
                }
            }

            zoon::println!("[DD_EVAL] Chained WhileRef for hold {} with {} arms", hold_id, evaluated_arms.len());

            return DdValue::WhileRef {
                hold_id: hold_id.clone(),
                computation: input_computation.clone(),  // Preserve computation from input
                arms: Arc::new(evaluated_arms),
                default: default_value,
            };
        }

        // Static evaluation for non-HoldRef inputs
        for arm in arms {
            if let Some(bindings) = self.match_pattern(value, &arm.pattern) {
                // Create new runtime with pattern bindings
                let mut match_runtime = BoonDdRuntime {
                    variables: self.variables.clone(),
                    functions: self.functions.clone(),
                    passed_context: self.passed_context.clone(),
                    link_counter: 0,
                    hold_counter: 0,
                    context_path: Vec::new(),
                };
                for (name, bound_value) in bindings {
                    match_runtime.variables.insert(name, bound_value);
                }
                return match_runtime.eval_expression(&arm.body.node);
            }
        }
        DdValue::Unit
    }

    /// Convert a pattern to a DdValue for WhileRef arms.
    /// Used to pre-evaluate pattern values for reactive matching.
    fn pattern_to_value(&mut self, pattern: &Pattern) -> Option<DdValue> {
        match pattern {
            Pattern::Literal(lit) => {
                Some(self.eval_literal(lit))
            }
            Pattern::TaggedObject { tag, .. } => {
                // For tag patterns without fields, create a simple tag value
                Some(DdValue::Tagged {
                    tag: Arc::from(tag.as_str()),
                    fields: Arc::new(BTreeMap::new()),
                })
            }
            Pattern::Alias { name } => {
                // Alias patterns match anything, use the name as identifier
                Some(DdValue::text(name.as_str()))
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
    fn match_pattern(&mut self, value: &DdValue, pattern: &Pattern) -> Option<Vec<(String, DdValue)>> {
        match pattern {
            Pattern::WildCard => Some(vec![]),

            Pattern::Alias { name } => {
                // Bind the value to the name
                Some(vec![(name.as_str().to_string(), value.clone())])
            }

            Pattern::Literal(lit) => {
                // Special case: Bool values matching True/False tag literals
                if let DdValue::Bool(b) = value {
                    if let Literal::Tag(tag_name) = lit {
                        let tag_str = tag_name.as_str();
                        if (tag_str == "True" && *b) || (tag_str == "False" && !*b) {
                            return Some(vec![]);
                        } else if tag_str == "True" || tag_str == "False" {
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
                if let DdValue::Bool(b) = value {
                    let tag_name = tag.as_str();
                    if (tag_name == "True" && *b) || (tag_name == "False" && !*b) {
                        return Some(vec![]);
                    } else {
                        return None;
                    }
                }

                if let DdValue::Tagged { tag: value_tag, fields } = value {
                    if tag.as_str() == value_tag.as_ref() {
                        // Match fields
                        let mut bindings = vec![];
                        for var in variables {
                            let field_value = fields.get(var.name.as_str()).cloned().unwrap_or(DdValue::Unit);
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
                if let DdValue::Object(fields) = value {
                    let mut bindings = vec![];
                    for var in variables {
                        let field_value = fields.get(var.name.as_str()).cloned().unwrap_or(DdValue::Unit);
                        bindings.push((var.name.as_str().to_string(), field_value));
                    }
                    Some(bindings)
                } else {
                    None
                }
            }

            Pattern::List { items } => {
                if let DdValue::List(list_items) = value {
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
    fn eval_comparator(&mut self, comp: &Comparator) -> DdValue {
        match comp {
            Comparator::Equal { operand_a, operand_b } => {
                let a = self.eval_expression(&operand_a.node);
                let b = self.eval_expression(&operand_b.node);
                // Handle reactive equality: WhileRef == static value
                // Returns a WhileRef where each arm's result is compared to the static value
                match (&a, &b) {
                    (DdValue::WhileRef { hold_id, computation, arms, default }, other) |
                    (other, DdValue::WhileRef { hold_id, computation, arms, default }) => {
                        // Create new arms where each result is compared with other
                        let new_arms: Vec<(DdValue, DdValue)> = arms.iter()
                            .map(|(pattern, result)| {
                                let eq_result = self.values_equal(result, other);
                                (pattern.clone(), DdValue::Bool(eq_result))
                            })
                            .collect();
                        let new_default = default.as_ref().map(|d| {
                            let eq_result = self.values_equal(d.as_ref(), other);
                            Arc::new(DdValue::Bool(eq_result))
                        });
                        #[cfg(debug_assertions)]
                        zoon::println!("[DD_EVAL] Reactive equality: WhileRef({}) == {:?} => WhileRef with {} arms", hold_id, other, new_arms.len());
                        DdValue::WhileRef {
                            hold_id: hold_id.clone(),
                            computation: computation.clone(),
                            arms: Arc::new(new_arms),
                            default: new_default,
                        }
                    }
                    // ComputedRef == ComputedRef => ComputedRef::Equal
                    // Used for: all_completed: completed_todos_count == todos_count
                    (DdValue::ComputedRef { source_hold, .. }, DdValue::ComputedRef { .. }) => {
                        DdValue::computed_ref(
                            ComputedType::Equal {
                                left: Box::new(a.clone()),
                                right: Box::new(b.clone()),
                            },
                            source_hold.clone(),
                        )
                    }
                    _ => {
                        #[cfg(debug_assertions)]
                        zoon::println!("[DD_EVAL] Comparing {:?} == {:?} => {:?}", a, b, a == b);
                        DdValue::Bool(a == b)
                    }
                }
            }
            Comparator::NotEqual { operand_a, operand_b } => {
                let a = self.eval_expression(&operand_a.node);
                let b = self.eval_expression(&operand_b.node);
                DdValue::Bool(a != b)
            }
            Comparator::Less { operand_a, operand_b } => {
                let a = self.eval_expression(&operand_a.node);
                let b = self.eval_expression(&operand_b.node);
                DdValue::Bool(a < b)
            }
            Comparator::Greater { operand_a, operand_b } => {
                let a = self.eval_expression(&operand_a.node);
                let b = self.eval_expression(&operand_b.node);
                match (&a, &b) {
                    // ComputedRef > 0 => ComputedRef::GreaterThanZero
                    // Used for: show_clear_completed: completed_todos_count > 0
                    (DdValue::ComputedRef { source_hold, .. }, DdValue::Number(n)) if n.0 == 0.0 => {
                        DdValue::computed_ref(
                            ComputedType::GreaterThanZero {
                                operand: Box::new(a.clone()),
                            },
                            source_hold.clone(),
                        )
                    }
                    _ => DdValue::Bool(a > b),
                }
            }
            Comparator::LessOrEqual { operand_a, operand_b } => {
                let a = self.eval_expression(&operand_a.node);
                let b = self.eval_expression(&operand_b.node);
                DdValue::Bool(a <= b)
            }
            Comparator::GreaterOrEqual { operand_a, operand_b } => {
                let a = self.eval_expression(&operand_a.node);
                let b = self.eval_expression(&operand_b.node);
                DdValue::Bool(a >= b)
            }
        }
    }

    /// Evaluate an arithmetic operator.
    fn eval_arithmetic(&mut self, op: &ArithmeticOperator) -> DdValue {
        match op {
            ArithmeticOperator::Add { operand_a, operand_b } => {
                let a = self.eval_expression(&operand_a.node);
                let b = self.eval_expression(&operand_b.node);
                match (&a, &b) {
                    (DdValue::Number(x), DdValue::Number(y)) => DdValue::float(x.0 + y.0),
                    (DdValue::Text(x), DdValue::Text(y)) => DdValue::text(format!("{}{}", x, y)),
                    _ => DdValue::Unit,
                }
            }
            ArithmeticOperator::Subtract { operand_a, operand_b } => {
                let a = self.eval_expression(&operand_a.node);
                let b = self.eval_expression(&operand_b.node);
                match (&a, &b) {
                    (DdValue::Number(x), DdValue::Number(y)) => DdValue::float(x.0 - y.0),
                    // ComputedRef - ComputedRef => ComputedRef::Subtract
                    // Used for: active_todos_count: todos_count - completed_todos_count
                    (DdValue::ComputedRef { source_hold, .. }, DdValue::ComputedRef { .. }) => {
                        DdValue::computed_ref(
                            ComputedType::Subtract {
                                left: Box::new(a.clone()),
                                right: Box::new(b.clone()),
                            },
                            source_hold.clone(),
                        )
                    }
                    _ => DdValue::Unit,
                }
            }
            ArithmeticOperator::Multiply { operand_a, operand_b } => {
                let a = self.eval_expression(&operand_a.node);
                let b = self.eval_expression(&operand_b.node);
                match (&a, &b) {
                    (DdValue::Number(x), DdValue::Number(y)) => DdValue::float(x.0 * y.0),
                    _ => DdValue::Unit,
                }
            }
            ArithmeticOperator::Divide { operand_a, operand_b } => {
                let a = self.eval_expression(&operand_a.node);
                let b = self.eval_expression(&operand_b.node);
                match (&a, &b) {
                    (DdValue::Number(x), DdValue::Number(y)) if y.0 != 0.0 => DdValue::float(x.0 / y.0),
                    _ => DdValue::Unit,
                }
            }
            ArithmeticOperator::Negate { operand } => {
                let a = self.eval_expression(&operand.node);
                match &a {
                    DdValue::Number(x) => DdValue::float(-x.0),
                    _ => DdValue::Unit,
                }
            }
        }
    }

    /// Evaluate an alias (variable reference with optional field path).
    fn eval_alias(&mut self, alias: &Alias) -> DdValue {
        match alias {
            Alias::WithoutPassed { parts, .. } => {
                if parts.is_empty() {
                    return DdValue::Unit;
                }

                // First part is the variable name
                let mut current = self
                    .variables
                    .get(parts[0].as_str())
                    .cloned()
                    .unwrap_or(DdValue::Unit);

                // Rest are field accesses
                for field in parts.iter().skip(1) {
                    current = current.get(field.as_str()).cloned().unwrap_or(DdValue::Unit);
                }

                current
            }
            Alias::WithPassed { extra_parts } => {
                // PASSED value - access the passed_context and navigate through fields
                let mut current = self.passed_context.clone().unwrap_or(DdValue::Unit);

                // Navigate through extra_parts (field accesses after PASSED)
                for field in extra_parts {
                    current = current.get(field.as_str()).cloned().unwrap_or(DdValue::Unit);
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
    fn replace_link_ref_in_value(&self, value: &DdValue, target_id: &Arc<str>) -> DdValue {
        use std::collections::BTreeMap;

        match value {
            // Replace any LinkRef with the target
            DdValue::LinkRef(_) => DdValue::LinkRef(target_id.clone()),

            // Recursively process objects
            DdValue::Object(fields) => {
                let new_fields: BTreeMap<Arc<str>, DdValue> = fields
                    .iter()
                    .map(|(k, v)| (k.clone(), self.replace_link_ref_in_value(v, target_id)))
                    .collect();
                DdValue::Object(Arc::new(new_fields))
            }

            // Recursively process tagged values
            DdValue::Tagged { tag, fields } => {
                let new_fields: BTreeMap<Arc<str>, DdValue> = fields
                    .iter()
                    .map(|(k, v)| (k.clone(), self.replace_link_ref_in_value(v, target_id)))
                    .collect();
                DdValue::Tagged {
                    tag: tag.clone(),
                    fields: Arc::new(new_fields),
                }
            }

            // Recursively process lists
            DdValue::List(items) => {
                let new_items: Vec<DdValue> = items
                    .iter()
                    .map(|item| self.replace_link_ref_in_value(item, target_id))
                    .collect();
                DdValue::List(Arc::new(new_items))
            }

            // Other values pass through unchanged
            _ => value.clone(),
        }
    }

    /// Compare two DdValues for equality.
    /// Handles Tagged comparison by comparing only the tag name.
    fn values_equal(&self, a: &DdValue, b: &DdValue) -> bool {
        match (a, b) {
            (DdValue::Tagged { tag: tag_a, .. }, DdValue::Tagged { tag: tag_b, .. }) => {
                tag_a.as_ref() == tag_b.as_ref()
            }
            (DdValue::Bool(a), DdValue::Bool(b)) => a == b,
            (DdValue::Number(a), DdValue::Number(b)) => a == b,
            (DdValue::Text(a), DdValue::Text(b)) => a.as_ref() == b.as_ref(),
            (DdValue::Unit, DdValue::Unit) => true,
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
pub fn evaluate_to_document(expressions: &[Spanned<Expression>]) -> Option<DdValue> {
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
        let val = DdValue::int(42);
        assert_eq!(val.to_display_string(), "42");

        let text = DdValue::text("hello");
        assert_eq!(text.to_display_string(), "hello");
    }
}
