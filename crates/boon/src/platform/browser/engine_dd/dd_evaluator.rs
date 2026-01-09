//! DD-based evaluator for Boon.
//!
//! This module evaluates Boon AST using simple value types.
//! This is phase 1: static evaluation without reactive features.
//! Phase 2 will add DD-based reactive evaluation.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use super::dd_value::DdValue;
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
}

impl BoonDdRuntime {
    /// Create a new DD runtime.
    pub fn new() -> Self {
        Self {
            variables: HashMap::new(),
            functions: HashMap::new(),
            passed_context: None,
        }
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
    fn eval_expression(&self, expr: &Expression) -> DdValue {
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
                let from_val = self.eval_expression(&from.node);
                self.eval_pipe(&from_val, &to.node)
            }

            // Block: BLOCK { vars, result }
            Expression::Block { variables, output } => {
                let mut block_runtime = BoonDdRuntime {
                    variables: self.variables.clone(),
                    functions: self.functions.clone(),
                    passed_context: self.passed_context.clone(),
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

            // LINK - for static eval, return unit
            Expression::Link => DdValue::Unit,

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
    fn eval_literal(&self, lit: &Literal) -> DdValue {
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
    fn eval_object(&self, obj: &Object) -> DdValue {
        // Create a scoped runtime with access to current variables
        let mut scoped_runtime = BoonDdRuntime {
            variables: self.variables.clone(),
            functions: self.functions.clone(),
            passed_context: self.passed_context.clone(),
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
    fn eval_text_literal(&self, parts: &[TextPart]) -> DdValue {
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
        &self,
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
    fn eval_user_function(&self, name: &str, args: &HashMap<&str, DdValue>) -> DdValue {
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
    fn eval_element_function(&self, name: &str, args: &HashMap<&str, DdValue>) -> DdValue {
        let mut fields: Vec<(&str, DdValue)> = vec![("_element_type", DdValue::text(name))];
        for (k, v) in args {
            fields.push((k, v.clone()));
        }
        DdValue::tagged("Element", fields.into_iter())
    }

    /// Evaluate a List function.
    fn eval_list_function(&self, name: &str, args: &HashMap<&str, DdValue>) -> DdValue {
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
    fn eval_router_function(&self, name: &str, _args: &HashMap<&str, DdValue>) -> DdValue {
        match name {
            // Router/route() - returns current URL path
            // In WASM context, get from browser; in static eval, return "/"
            "route" => {
                #[cfg(target_arch = "wasm32")]
                {
                    use zoon::*;
                    let path = window().location().pathname().unwrap_or_else(|_| "/".to_string());
                    DdValue::text(path)
                }
                #[cfg(not(target_arch = "wasm32"))]
                {
                    DdValue::text("/")
                }
            }
            // Router/go_to(route) - navigation (no-op in static context)
            "go_to" => DdValue::Unit,
            _ => DdValue::Unit,
        }
    }

    /// Evaluate a Text function.
    fn eval_text_function(&self, name: &str, args: &HashMap<&str, DdValue>) -> DdValue {
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
    fn eval_pipe(&self, from: &DdValue, to: &Expression) -> DdValue {
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

                        if let (Some(binding), Some(pred_expr), DdValue::List(items)) =
                            (binding_name, predicate_expr, from)
                        {
                            let filtered: Vec<DdValue> = items
                                .iter()
                                .filter(|item| {
                                    // Create scoped runtime with the item bound
                                    let mut scoped = BoonDdRuntime {
                                        variables: self.variables.clone(),
                                        functions: self.functions.clone(),
                                        passed_context: self.passed_context.clone(),
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
                        if let DdValue::List(items) = from {
                            DdValue::int(items.len() as i64)
                        } else {
                            DdValue::int(0)
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

            // Pipe to LINK { alias } - pass through the value
            // In Boon, `element |> LINK { alias }` registers the element for event handling
            // and returns the element unchanged
            Expression::LinkSetter { .. } => from.clone(),

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
    fn eval_hold(&self, initial: &DdValue, state_name: &str, body: &Expression) -> DdValue {
        // Try to extract pulse count from body: `count |> Stream/pulses() |> ...`
        let pulse_count = self.extract_pulse_count(body);

        if pulse_count == 0 {
            // No Stream/pulses, just return initial value
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
    fn extract_pulse_count(&self, expr: &Expression) -> i64 {
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

    /// Evaluate pattern matching for WHEN/WHILE.
    fn eval_pattern_match(&self, value: &DdValue, arms: &[Arm]) -> DdValue {
        for arm in arms {
            if let Some(bindings) = self.match_pattern(value, &arm.pattern) {
                // Create new runtime with pattern bindings
                let mut match_runtime = BoonDdRuntime {
                    variables: self.variables.clone(),
                    functions: self.functions.clone(),
                    passed_context: self.passed_context.clone(),
                };
                for (name, bound_value) in bindings {
                    match_runtime.variables.insert(name, bound_value);
                }
                return match_runtime.eval_expression(&arm.body.node);
            }
        }
        DdValue::Unit
    }

    /// Try to match a value against a pattern, returning bindings if successful.
    fn match_pattern(&self, value: &DdValue, pattern: &Pattern) -> Option<Vec<(String, DdValue)>> {
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
    fn eval_comparator(&self, comp: &Comparator) -> DdValue {
        match comp {
            Comparator::Equal { operand_a, operand_b } => {
                let a = self.eval_expression(&operand_a.node);
                let b = self.eval_expression(&operand_b.node);
                #[cfg(debug_assertions)]
                zoon::println!("[DD_EVAL] Comparing {:?} == {:?} => {:?}", a, b, a == b);
                DdValue::Bool(a == b)
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
                DdValue::Bool(a > b)
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
    fn eval_arithmetic(&self, op: &ArithmeticOperator) -> DdValue {
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
    fn eval_alias(&self, alias: &Alias) -> DdValue {
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
