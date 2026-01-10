//! Reactive evaluation for DD engine.
//!
//! This module extends the static DD evaluator with reactive capabilities.
//! It processes HOLD, LINK, and THEN expressions to create a reactive UI.
//!
//! # Architecture
//!
//! ```text
//! Boon Source → Parser → Static AST
//!                           ↓
//!                  DdReactiveEvaluator
//!                  ├── Evaluates expressions
//!                  ├── Creates DdSignals for HOLD state
//!                  ├── Creates Links for LINK expressions
//!                  └── Connects THEN to state updates
//!                           ↓
//!                  (DdValue document, DdReactiveContext)
//!                           ↓
//!                  Bridge renders with reactive bindings
//! ```

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::Arc;

use zoon::{Mutable, MutableExt};
#[cfg(target_arch = "wasm32")]
use zoon::WebStorage;

use super::dd_link::{Link, LinkId, LinkRegistry};
use super::dd_stream::DdSignal;
use super::dd_value::DdValue;
use crate::parser::static_expression::{
    Alias, Arm, ArithmeticOperator, Comparator, Expression, Literal, Object, Pattern, Spanned,
    TextPart,
};

/// A unique identifier for HOLD state.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct HoldId(pub String);

impl HoldId {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }
}

/// Reactive context that manages state and event bindings.
///
/// This is the generic replacement for the hardcoded ReactiveContext.
#[derive(Clone)]
pub struct DdReactiveContext {
    /// HOLD state signals, keyed by variable name
    holds: Rc<RefCell<HashMap<HoldId, DdSignal>>>,
    /// LINK event registry
    links: Rc<LinkRegistry>,
    /// Render trigger - increment to force re-render
    render_trigger: Rc<Mutable<u64>>,
    /// Current storage prefix for persistence
    storage_prefix: Option<String>,
}

impl DdReactiveContext {
    pub fn new() -> Self {
        Self {
            holds: Rc::new(RefCell::new(HashMap::new())),
            links: Rc::new(LinkRegistry::new()),
            render_trigger: Rc::new(Mutable::new(0)),
            storage_prefix: None,
        }
    }

    pub fn new_with_persistence(prefix: impl Into<String>) -> Self {
        Self {
            holds: Rc::new(RefCell::new(HashMap::new())),
            links: Rc::new(LinkRegistry::new()),
            render_trigger: Rc::new(Mutable::new(0)),
            storage_prefix: Some(prefix.into()),
        }
    }

    /// Register or get a HOLD state signal.
    pub fn register_hold(&self, id: HoldId, initial: DdValue) -> DdSignal {
        let mut holds = self.holds.borrow_mut();
        if let Some(signal) = holds.get(&id) {
            return signal.clone();
        }

        // Try to load from localStorage if persistence enabled
        let loaded_value = if let Some(ref prefix) = self.storage_prefix {
            let key = format!("dd_{}_{}", prefix, id.0);
            #[cfg(target_arch = "wasm32")]
            {
                use zoon::local_storage;
                if let Some(Ok(json)) = local_storage().get::<String>(&key) {
                    // Try to deserialize the value
                    // For numbers, parse as f64
                    if let Ok(n) = json.parse::<f64>() {
                        Some(DdValue::float(n))
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                None
            }
        } else {
            None
        };

        let signal = DdSignal::new(loaded_value.unwrap_or(initial));
        holds.insert(id, signal.clone());
        signal
    }

    /// Get an existing HOLD signal.
    pub fn get_hold(&self, id: &HoldId) -> Option<DdSignal> {
        self.holds.borrow().get(id).cloned()
    }

    /// Update a HOLD signal's value.
    pub fn update_hold(&self, id: &HoldId, value: DdValue) {
        if let Some(signal) = self.holds.borrow().get(id) {
            signal.set(value.clone());

            // Save to localStorage if persistence enabled
            if let Some(ref prefix) = self.storage_prefix {
                let key = format!("dd_{}_{}", prefix, id.0);
                #[cfg(target_arch = "wasm32")]
                {
                    use zoon::local_storage;
                    if let DdValue::Number(n) = &value {
                        let _ = local_storage().insert(&key, &n.0.to_string());
                    }
                }
            }
        }
    }

    /// Register a LINK and get it.
    pub fn register_link(&self, id: LinkId) -> Link {
        self.links.register(id)
    }

    /// Get an existing LINK.
    pub fn get_link(&self, id: &LinkId) -> Option<Link> {
        self.links.get(id)
    }

    /// Fire a LINK event.
    pub fn fire_link(&self, id: &LinkId) {
        self.links.fire_unit(id);
    }

    /// Fire a LINK event with a value.
    pub fn fire_link_with_value(&self, id: &LinkId, value: DdValue) {
        self.links.fire(id, value);
    }

    /// Trigger a re-render.
    pub fn trigger_render(&self) {
        self.render_trigger.update(|v| v + 1);
    }

    /// Get the render trigger signal.
    /// Returns a cloned signal that doesn't borrow self.
    pub fn render_signal(&self) -> impl zoon::Signal<Item = u64> + 'static {
        let trigger = (*self.render_trigger).clone();
        trigger.signal()
    }

    /// Get the link registry.
    pub fn link_registry(&self) -> &LinkRegistry {
        &self.links
    }

    /// Get all HOLD values for rendering.
    pub fn get_hold_values(&self) -> HashMap<String, DdValue> {
        self.holds
            .borrow()
            .iter()
            .map(|(id, signal)| (id.0.clone(), signal.get()))
            .collect()
    }
}

impl Default for DdReactiveContext {
    fn default() -> Self {
        Self::new()
    }
}

/// Reactive evaluator for DD engine.
///
/// This evaluator processes expressions and populates a DdReactiveContext
/// with HOLD states, LINK bindings, and THEN connections.
pub struct DdReactiveEvaluator {
    /// Variable values (static snapshot)
    variables: HashMap<String, DdValue>,
    /// Function definitions
    functions: HashMap<String, FunctionDef>,
    /// PASSED context for function calls
    passed_context: Option<DdValue>,
    /// Reactive context for HOLD/LINK
    reactive_ctx: DdReactiveContext,
    /// THEN connections: when this LINK fires, run this computation
    then_connections: Vec<ThenConnection>,
}

#[derive(Clone)]
struct FunctionDef {
    parameters: Vec<String>,
    body: Box<Spanned<Expression>>,
}

/// A connection between a LINK and a HOLD via THEN.
struct ThenConnection {
    /// The LINK that triggers this
    link_id: LinkId,
    /// The HOLD to update
    hold_id: HoldId,
    /// The state parameter name
    state_name: String,
    /// The body expression to evaluate
    body: Box<Spanned<Expression>>,
}

impl DdReactiveEvaluator {
    /// Create a new reactive evaluator.
    pub fn new() -> Self {
        Self {
            variables: HashMap::new(),
            functions: HashMap::new(),
            passed_context: None,
            reactive_ctx: DdReactiveContext::new(),
            then_connections: Vec::new(),
        }
    }

    /// Create a new reactive evaluator with persistence.
    pub fn new_with_persistence(prefix: impl Into<String>) -> Self {
        Self {
            variables: HashMap::new(),
            functions: HashMap::new(),
            passed_context: None,
            reactive_ctx: DdReactiveContext::new_with_persistence(prefix),
            then_connections: Vec::new(),
        }
    }

    /// Get the reactive context.
    pub fn reactive_context(&self) -> &DdReactiveContext {
        &self.reactive_ctx
    }

    /// Consume the evaluator and return the reactive context.
    pub fn into_reactive_context(self) -> DdReactiveContext {
        self.reactive_ctx
    }

    /// Get all variables.
    pub fn get_all_variables(&self) -> &HashMap<String, DdValue> {
        &self.variables
    }

    /// Get the document output.
    pub fn get_document(&self) -> Option<&DdValue> {
        self.variables.get("document")
    }

    /// Inject a variable value before evaluation.
    pub fn inject_variable(&mut self, name: impl Into<String>, value: DdValue) {
        self.variables.insert(name.into(), value);
    }

    /// Evaluate expressions and populate reactive context.
    pub fn evaluate(&mut self, expressions: &[Spanned<Expression>]) {
        // Remember which variables were pre-injected
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

        // First pass: evaluate all variables
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
        for expr in expressions {
            if let Expression::Variable(var) = &expr.node {
                let name = var.name.as_str().to_string();
                if injected_vars.contains(&name) {
                    continue;
                }
                let value = self.eval_expression(&var.value.node);
                self.variables.insert(name, value);
            }
        }
    }

    /// Evaluate a single expression.
    fn eval_expression(&mut self, expr: &Expression) -> DdValue {
        match expr {
            Expression::Literal(lit) => self.eval_literal(lit),
            Expression::Alias(alias) => self.eval_alias(alias),
            Expression::Object(obj) => self.eval_object(obj),
            Expression::List { items } => {
                let values: Vec<DdValue> = items
                    .iter()
                    .map(|spanned| self.eval_expression(&spanned.node))
                    .collect();
                DdValue::list(values)
            }
            Expression::TextLiteral { parts } => self.eval_text_literal(parts),
            Expression::FunctionCall { path, arguments } => {
                self.eval_function_call(path, arguments)
            }
            Expression::Pipe { from, to } => {
                let from_val = self.eval_expression(&from.node);
                self.eval_pipe(&from_val, &to.node)
            }
            Expression::Block { variables, output } => {
                let saved_vars = self.variables.clone();
                for var in variables {
                    let name = var.node.name.as_str().to_string();
                    let value = self.eval_expression(&var.node.value.node);
                    self.variables.insert(name, value);
                }
                let result = self.eval_expression(&output.node);
                self.variables = saved_vars;
                result
            }
            Expression::Comparator(comp) => self.eval_comparator(comp),
            Expression::ArithmeticOperator(op) => self.eval_arithmetic(op),
            Expression::Latest { inputs } => {
                for input in inputs {
                    let val = self.eval_expression(&input.node);
                    if val != DdValue::Unit {
                        return val;
                    }
                }
                DdValue::Unit
            }
            Expression::Hold { .. } => DdValue::Unit, // Handled in eval_pipe
            Expression::Then { .. } => DdValue::Unit, // Handled in eval_pipe
            Expression::When { arms } | Expression::While { arms } => DdValue::Unit,
            Expression::Link => DdValue::Unit,
            Expression::Skip => DdValue::Unit,
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
            Expression::Variable(var) => self.eval_expression(&var.value.node),
            Expression::FieldAccess { .. } => DdValue::Unit,
            _ => DdValue::Unit,
        }
    }

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

    fn eval_object(&mut self, obj: &Object) -> DdValue {
        let mut map = BTreeMap::new();
        for var in &obj.variables {
            let name: Arc<str> = Arc::from(var.node.name.as_str());
            let value = self.eval_expression(&var.node.value.node);
            map.insert(name.clone(), value.clone());
            self.variables.insert(var.node.name.as_str().to_string(), value);
        }
        DdValue::Object(Arc::new(map))
    }

    fn eval_text_literal(&self, parts: &[TextPart]) -> DdValue {
        let mut result = String::new();
        for part in parts {
            match part {
                TextPart::Text(s) => result.push_str(s.as_str()),
                TextPart::Interpolation { var, .. } => {
                    let var_name = var.as_str();
                    // First check reactive HOLD values
                    let hold_id = HoldId::new(var_name);
                    if let Some(signal) = self.reactive_ctx.get_hold(&hold_id) {
                        result.push_str(&signal.get().to_display_string());
                    } else if let Some(value) = self.variables.get(var_name) {
                        result.push_str(&value.to_display_string());
                    }
                }
            }
        }
        DdValue::text(result)
    }

    fn eval_function_call(
        &mut self,
        path: &[crate::parser::StrSlice],
        arguments: &[Spanned<crate::parser::static_expression::Argument>],
    ) -> DdValue {
        let args: HashMap<&str, DdValue> = arguments
            .iter()
            .filter_map(|arg| {
                let name = arg.node.name.as_str();
                let value = arg.node.value.as_ref().map(|v| self.eval_expression(&v.node))?;
                Some((name, value))
            })
            .collect();

        let full_path: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
        let (namespace, name) = if full_path.len() >= 2 {
            (Some(full_path[0]), full_path[1])
        } else if full_path.len() == 1 {
            (None, full_path[0])
        } else {
            return DdValue::Unit;
        };

        match (namespace, name) {
            (Some("Document"), "new") => args.get("root").cloned().unwrap_or(DdValue::Unit),
            (Some("Math"), "sum") => DdValue::int(0),
            (Some("Timer"), "interval") => DdValue::Unit,
            (Some("Stream"), "pulses") => DdValue::Unit,
            (Some("Element"), func) => self.eval_element_function(func, &args),
            (Some("List"), func) => self.eval_list_function(func, &args),
            (Some("Router"), func) => self.eval_router_function(func, &args),
            (Some("Text"), func) => self.eval_text_function(func, &args),
            (None, func_name) => self.eval_user_function(func_name, &args),
            _ => DdValue::Unit,
        }
    }

    fn eval_element_function(&mut self, name: &str, args: &HashMap<&str, DdValue>) -> DdValue {
        let mut fields: Vec<(&str, DdValue)> = vec![("_element_type", DdValue::text(name))];
        for (k, v) in args {
            fields.push((k, v.clone()));
        }
        DdValue::tagged("Element", fields.into_iter())
    }

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

    fn eval_router_function(&self, name: &str, _args: &HashMap<&str, DdValue>) -> DdValue {
        match name {
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
            "go_to" => DdValue::Unit,
            _ => DdValue::Unit,
        }
    }

    fn eval_text_function(&self, name: &str, args: &HashMap<&str, DdValue>) -> DdValue {
        match name {
            "trim" => {
                if let Some(DdValue::Text(s)) = args.values().next() {
                    DdValue::text(s.trim())
                } else {
                    DdValue::text("")
                }
            }
            "is_not_empty" => {
                if let Some(DdValue::Text(s)) = args.values().next() {
                    DdValue::Bool(!s.is_empty())
                } else {
                    DdValue::Bool(false)
                }
            }
            "is_empty" => {
                if let Some(DdValue::Text(s)) = args.values().next() {
                    DdValue::Bool(s.is_empty())
                } else {
                    DdValue::Bool(true)
                }
            }
            "empty" => DdValue::text(""),
            _ => DdValue::Unit,
        }
    }

    fn eval_user_function(&mut self, name: &str, args: &HashMap<&str, DdValue>) -> DdValue {
        if let Some(func_def) = self.functions.get(name).cloned() {
            let passed_context = args.get("PASS").cloned().or_else(|| self.passed_context.clone());
            let saved_vars = self.variables.clone();
            let saved_passed = self.passed_context.clone();

            self.passed_context = passed_context;

            for (param, arg_name) in func_def.parameters.iter().zip(args.keys()) {
                if let Some(value) = args.get(*arg_name) {
                    self.variables.insert(param.clone(), value.clone());
                }
            }
            for param in &func_def.parameters {
                if let Some(value) = args.get(param.as_str()) {
                    self.variables.insert(param.clone(), value.clone());
                }
            }

            let result = self.eval_expression(&func_def.body.node);
            self.variables = saved_vars;
            self.passed_context = saved_passed;
            result
        } else {
            DdValue::Unit
        }
    }

    fn eval_pipe(&mut self, from: &DdValue, to: &Expression) -> DdValue {
        match to {
            Expression::FunctionCall { path, arguments } => {
                self.eval_pipe_to_function_call(from, path, arguments)
            }
            Expression::Hold { state_param, body } => {
                self.eval_pipe_to_hold(from, state_param.as_str(), &body.node)
            }
            Expression::Then { body } => {
                // THEN without piped LINK - evaluate body once
                self.eval_expression(&body.node)
            }
            Expression::When { arms } => self.eval_pattern_match(from, arms),
            Expression::While { arms } => self.eval_pattern_match(from, arms),
            Expression::FieldAccess { path } => {
                let mut current = from.clone();
                for field in path {
                    current = current.get(field.as_str()).cloned().unwrap_or(DdValue::Unit);
                }
                current
            }
            Expression::LinkSetter { .. } => from.clone(),
            _ => self.eval_expression(to),
        }
    }

    fn eval_pipe_to_function_call(
        &mut self,
        from: &DdValue,
        path: &[crate::parser::StrSlice],
        arguments: &[Spanned<crate::parser::static_expression::Argument>],
    ) -> DdValue {
        let full_path: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
        let (namespace, name) = if full_path.len() >= 2 {
            (Some(full_path[0]), full_path[1])
        } else if full_path.len() == 1 {
            (None, full_path[0])
        } else {
            return DdValue::Unit;
        };

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
                if !args.contains_key("root") {
                    return from.clone();
                }
                args.get("root").cloned().unwrap_or(DdValue::Unit)
            }
            (Some("Math"), "sum") => from.clone(),
            (Some("Stream"), "skip") => from.clone(),
            (Some("Log"), "info") => from.clone(),
            (Some("List"), "append") => from.clone(),
            (Some("List"), "remove") => from.clone(),
            (Some("List"), "count") => {
                if let DdValue::List(items) = from {
                    DdValue::int(items.len() as i64)
                } else {
                    DdValue::int(0)
                }
            }
            (Some("Bool"), "or") => {
                let from_bool = from.is_truthy();
                let that_bool = args.get("that").map(|v| v.is_truthy()).unwrap_or(false);
                DdValue::Bool(from_bool || that_bool)
            }
            (Some("Bool"), "and") => {
                let from_bool = from.is_truthy();
                let that_bool = args.get("that").map(|v| v.is_truthy()).unwrap_or(true);
                DdValue::Bool(from_bool && that_bool)
            }
            (Some("Bool"), "not") => DdValue::Bool(!from.is_truthy()),
            (Some("Text"), "trim") => {
                if let DdValue::Text(s) = from {
                    DdValue::text(s.trim())
                } else {
                    from.clone()
                }
            }
            (None, func_name) => self.eval_user_function_with_piped(func_name, from, &args),
            _ => self.eval_expression(&Expression::FunctionCall {
                path: path.to_vec(),
                arguments: arguments.to_vec(),
            }),
        }
    }

    fn eval_user_function_with_piped(
        &mut self,
        name: &str,
        piped: &DdValue,
        args: &HashMap<&str, DdValue>,
    ) -> DdValue {
        if let Some(func_def) = self.functions.get(name).cloned() {
            let passed_context = args.get("PASS").cloned().or_else(|| self.passed_context.clone());
            let saved_vars = self.variables.clone();
            let saved_passed = self.passed_context.clone();

            self.passed_context = passed_context;

            if let Some(first_param) = func_def.parameters.first() {
                self.variables.insert(first_param.clone(), piped.clone());
            }
            for param in &func_def.parameters {
                if let Some(value) = args.get(param.as_str()) {
                    self.variables.insert(param.clone(), value.clone());
                }
            }

            let result = self.eval_expression(&func_def.body.node);
            self.variables = saved_vars;
            self.passed_context = saved_passed;
            result
        } else {
            DdValue::Unit
        }
    }

    /// Evaluate HOLD - creates reactive state.
    fn eval_pipe_to_hold(&mut self, initial: &DdValue, state_name: &str, body: &Expression) -> DdValue {
        // Create a HoldId for this HOLD
        let hold_id = HoldId::new(state_name);

        // Register the HOLD with its initial value
        let signal = self.reactive_ctx.register_hold(hold_id.clone(), initial.clone());

        // Check if body has LINK |> THEN pattern
        if let Expression::Pipe { from, to } = body {
            if let Expression::Alias(alias) = &from.node {
                // Get the LINK variable name
                let link_name = match alias {
                    Alias::WithoutPassed { parts, .. } if !parts.is_empty() => {
                        parts[0].as_str().to_string()
                    }
                    _ => String::new(),
                };

                if !link_name.is_empty() {
                    // Register the LINK
                    let link_id = LinkId::new(&link_name);
                    self.reactive_ctx.register_link(link_id.clone());

                    // If to is THEN, set up the connection
                    if let Expression::Then { body: then_body } = &to.node {
                        // Store the connection for later wiring
                        self.then_connections.push(ThenConnection {
                            link_id,
                            hold_id: hold_id.clone(),
                            state_name: state_name.to_string(),
                            body: then_body.clone(),
                        });
                    }
                }
            }
        }

        // Also handle static Stream/pulses iteration for initial value computation
        let pulse_count = self.extract_pulse_count(body);
        if pulse_count > 0 {
            let then_body = self.extract_then_body(body);
            if let Some(then_body) = then_body {
                // Iterate to compute initial value
                let mut current_state = initial.clone();
                for _ in 0..pulse_count {
                    let saved_vars = self.variables.clone();
                    self.variables.insert(state_name.to_string(), current_state.clone());
                    let next_state = self.eval_expression(then_body);
                    self.variables = saved_vars;
                    if next_state != DdValue::Unit {
                        current_state = next_state;
                    }
                }
                // Update the signal with computed value
                signal.set(current_state.clone());
                return current_state;
            }
        }

        // Return a HoldRef so the bridge can look up the current value at render time
        DdValue::HoldRef(Arc::from(state_name))
    }

    fn extract_pulse_count(&self, expr: &Expression) -> i64 {
        match expr {
            Expression::Pipe { from, to } => {
                if self.is_stream_pulses(&to.node) {
                    if let Expression::Literal(Literal::Number(n)) = &from.node {
                        return (*n as i64).max(0);
                    }
                }
                let from_count = self.extract_pulse_count(&from.node);
                if from_count > 0 {
                    return from_count;
                }
                self.extract_pulse_count(&to.node)
            }
            _ => 0,
        }
    }

    fn is_stream_pulses(&self, expr: &Expression) -> bool {
        if let Expression::FunctionCall { path, .. } = expr {
            let parts: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
            return parts == vec!["Stream", "pulses"];
        }
        false
    }

    fn extract_then_body<'a>(&self, expr: &'a Expression) -> Option<&'a Expression> {
        match expr {
            Expression::Pipe { to, .. } => {
                if let Expression::Then { body } = &to.node {
                    return Some(&body.node);
                }
                self.extract_then_body(&to.node)
            }
            Expression::Then { body } => Some(&body.node),
            _ => None,
        }
    }

    fn eval_pattern_match(&mut self, value: &DdValue, arms: &[Arm]) -> DdValue {
        for arm in arms {
            if let Some(bindings) = self.match_pattern(value, &arm.pattern) {
                let saved_vars = self.variables.clone();
                for (name, bound_value) in bindings {
                    self.variables.insert(name, bound_value);
                }
                let result = self.eval_expression(&arm.body.node);
                self.variables = saved_vars;
                return result;
            }
        }
        DdValue::Unit
    }

    fn match_pattern(&self, value: &DdValue, pattern: &Pattern) -> Option<Vec<(String, DdValue)>> {
        match pattern {
            Pattern::WildCard => Some(vec![]),
            Pattern::Alias { name } => {
                Some(vec![(name.as_str().to_string(), value.clone())])
            }
            Pattern::Literal(lit) => {
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
            Pattern::Map { .. } => None,
        }
    }

    fn eval_comparator(&mut self, comp: &Comparator) -> DdValue {
        match comp {
            Comparator::Equal { operand_a, operand_b } => {
                let a = self.eval_expression(&operand_a.node);
                let b = self.eval_expression(&operand_b.node);
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

    fn eval_alias(&self, alias: &Alias) -> DdValue {
        match alias {
            Alias::WithoutPassed { parts, .. } => {
                if parts.is_empty() {
                    return DdValue::Unit;
                }
                let var_name = parts[0].as_str();

                // First check reactive HOLD values - return HoldRef for dynamic lookup
                let hold_id = HoldId::new(var_name);
                let mut current = if self.reactive_ctx.get_hold(&hold_id).is_some() {
                    // Return a HoldRef so the value is looked up at render time
                    if parts.len() == 1 {
                        // Simple reference to the HOLD itself
                        return DdValue::HoldRef(Arc::from(var_name));
                    }
                    // For field access, get current value (TODO: support nested HoldRef)
                    self.reactive_ctx.get_hold(&hold_id).unwrap().get()
                } else {
                    self.variables.get(var_name).cloned().unwrap_or(DdValue::Unit)
                };

                for field in parts.iter().skip(1) {
                    current = current.get(field.as_str()).cloned().unwrap_or(DdValue::Unit);
                }
                current
            }
            Alias::WithPassed { extra_parts } => {
                let mut current = self.passed_context.clone().unwrap_or(DdValue::Unit);
                for field in extra_parts {
                    current = current.get(field.as_str()).cloned().unwrap_or(DdValue::Unit);
                }
                current
            }
        }
    }
}

impl Default for DdReactiveEvaluator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reactive_context_creation() {
        let ctx = DdReactiveContext::new();
        assert!(ctx.get_hold(&HoldId::new("test")).is_none());
    }

    #[test]
    fn test_hold_registration() {
        let ctx = DdReactiveContext::new();
        let id = HoldId::new("counter");
        let signal = ctx.register_hold(id.clone(), DdValue::int(42));
        assert_eq!(signal.get(), DdValue::int(42));

        // Second registration returns same signal
        let signal2 = ctx.register_hold(id.clone(), DdValue::int(0));
        assert_eq!(signal2.get(), DdValue::int(42));
    }
}
