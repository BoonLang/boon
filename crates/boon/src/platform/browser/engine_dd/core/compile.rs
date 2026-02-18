//! Boon source → compiled program.
//!
//! Parses Boon source code and compiles it into a `CompiledProgram`:
//! - Static programs (no LINK/HOLD/Timer) → evaluated to a Value
//! - Reactive programs → DataflowGraph with CollectionSpec entries
//!
//! Uses the existing Boon parser. The compiler walks the static AST
//! and evaluates/compiles expressions.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use indexmap::IndexMap;

use crate::parser::{
    lexer, parser, reset_expression_depth, resolve_references, span_at,
    static_expression::{
        self, Alias, Argument, ArithmeticOperator, Expression, Literal, Spanned, TextPart,
    },
    SourceCode,
};

use super::types::{
    BroadcastHandlerFn, CollectionSpec, DataflowGraph, InputId, InputKind, InputSpec,
    KeyedListOutput, ListKey, SideEffectKind, VarId,
};
use super::value::Value;

/// Result of compiling a Boon program.
pub enum CompiledProgram {
    /// Purely static program — no reactive computation.
    Static { document_value: Value },

    /// Reactive program compiled to a DD dataflow graph.
    /// All reactive computation flows through DD collections and operators.
    Dataflow { graph: DataflowGraph },
}

/// A binding from a LINK path to a DD input.
pub struct LinkBinding {
    pub link_id: super::types::LinkId,
    pub input_id: InputId,
}

/// Operations collected from a `LIST {} |> List/append(...) |> List/remove(...)` chain.
enum ListChainOp<'a> {
    /// `List/append(item: source_expr)`
    Append(&'a [Spanned<Argument>]),
    /// `List/remove(item, on: condition_expr)`
    Remove(&'a [Spanned<Argument>]),
    /// `List/clear(on: event_source)`
    Clear(&'a [Spanned<Argument>]),
}

/// Compile Boon source code into a program.
pub fn compile(
    source_code: &str,
    storage_key: Option<&str>,
    persisted_holds: &std::collections::HashMap<String, Value>,
) -> Result<CompiledProgram, String> {
    let ast = parse_source(source_code)?;

    // Build top-level scope
    let mut compiler = Compiler::new();
    compiler.register_top_level(&ast);

    // Find the document variable
    let doc_expr = compiler
        .get_var_expr("document")
        .ok_or_else(|| "No 'document' variable found".to_string())?
        .clone();

    // Check if program is reactive
    if compiler.has_reactive_constructs() {
        // Compile to DataflowGraph
        match compiler.compile_to_graph(&doc_expr, storage_key, persisted_holds) {
            Ok(graph) => Ok(CompiledProgram::Dataflow { graph }),
            Err(e) => Err(format!("Reactive compilation failed: {}", e)),
        }
    } else {
        // Try static evaluation
        match compiler.eval_static(&doc_expr) {
            Ok(value) => Ok(CompiledProgram::Static {
                document_value: value,
            }),
            Err(e) => Err(format!("Static evaluation failed: {}", e)),
        }
    }
}

// ---------------------------------------------------------------------------
// Parser integration
// ---------------------------------------------------------------------------

fn parse_source(source_code: &str) -> Result<Vec<Spanned<Expression>>, String> {
    use chumsky::prelude::*;

    let source_code_arc = SourceCode::new(source_code.to_string());
    let source_for_parsing = source_code_arc.clone();
    let source_ref = source_for_parsing.as_str();

    let (tokens, lex_errors) = lexer().parse(source_ref).into_output_errors();
    if !lex_errors.is_empty() {
        return Err(format!("Lex errors: {:?}", lex_errors));
    }
    let Some(mut tokens) = tokens else {
        return Err("Lexer produced no output".to_string());
    };

    // Remove comments
    tokens.retain(|t| !matches!(t.node, crate::parser::Token::Comment(_)));

    reset_expression_depth();
    let (ast, parse_errors) = parser()
        .parse(tokens.map(
            span_at(source_ref.len()),
            |crate::parser::Spanned {
                 node,
                 span,
                 persistence: _,
             }| { (node, span) },
        ))
        .into_output_errors();

    if !parse_errors.is_empty() {
        return Err(format!("Parse errors: {:?}", parse_errors));
    }
    let Some(ast) = ast else {
        return Err("Parser produced no output".to_string());
    };

    let ast = resolve_references(ast).map_err(|e| format!("Reference errors: {:?}", e))?;

    // Convert to static expressions
    let static_ast = static_expression::convert_expressions(source_code_arc, ast);
    Ok(static_ast)
}

// ---------------------------------------------------------------------------
// Compiler
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Compiler {
    /// Top-level variable definitions: name → expression
    variables: Vec<(String, Spanned<Expression>)>,
    /// Function definitions: name → (params, body)
    functions: Vec<(String, Vec<String>, Spanned<Expression>)>,
}

impl Compiler {
    fn new() -> Self {
        Self {
            variables: Vec::new(),
            functions: Vec::new(),
        }
    }

    fn register_top_level(&mut self, ast: &[Spanned<Expression>]) {
        for expr in ast {
            match &expr.node {
                Expression::Variable(var) => {
                    let name = var.name.as_str().to_string();
                    self.variables.push((name.clone(), var.value.clone()));
                    // Flatten object fields into dotted-name variables.
                    // This allows sibling references (e.g., text_to_add inside store)
                    // to be resolved during state updates.
                    self.flatten_object_fields(&name, &var.value);
                }
                Expression::Function {
                    name,
                    parameters,
                    body,
                } => {
                    let fn_name = name.as_str().to_string();
                    let params: Vec<String> =
                        parameters.iter().map(|p| p.node.as_str().to_string()).collect();
                    self.functions
                        .push((fn_name, params, body.as_ref().clone()));
                }
                _ => {}
            }
        }
    }

    /// Recursively flatten object fields into dotted-name variables.
    fn flatten_object_fields(&mut self, prefix: &str, expr: &Spanned<Expression>) {
        if let Expression::Object(obj) = &expr.node {
            for var in &obj.variables {
                let field_name = format!("{}.{}", prefix, var.node.name.as_str());
                self.variables.push((field_name.clone(), var.node.value.clone()));
                // Recurse for nested objects
                self.flatten_object_fields(&field_name, &var.node.value);
            }
        }
    }

    fn get_var_expr(&self, name: &str) -> Option<&Spanned<Expression>> {
        self.variables
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, e)| e)
    }

    fn has_reactive_constructs(&self) -> bool {
        // Only needs DataflowGraph if there are external inputs (LINK, Timer)
        // Programs with HOLD/WHILE but no external inputs (like fibonacci)
        // can be evaluated statically.
        self.variables.iter().any(|(_, expr)| Self::has_external_input(expr))
    }

    fn has_external_input(expr: &Spanned<Expression>) -> bool {
        match &expr.node {
            Expression::Link | Expression::LinkSetter { .. } => true,
            Expression::Variable(var) => Self::has_external_input(&var.value),
            Expression::Pipe { from, to } => Self::has_external_input(from) || Self::has_external_input(to),
            Expression::FunctionCall { path, arguments } => {
                let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                if matches!(path_strs.as_slice(), ["Timer", "interval"] | ["Router", "route"] | ["Router", "go_to"]) {
                    return true;
                }
                arguments.iter().any(|a| {
                    a.node.value.as_ref()
                        .map(|v| Self::has_external_input(v))
                        .unwrap_or(false)
                })
            }
            Expression::List { items } => items.iter().any(Self::has_external_input),
            Expression::Object(obj) => obj.variables.iter().any(|v| Self::has_external_input(&v.node.value)),
            Expression::Block { variables, output } => {
                variables.iter().any(|v| Self::has_external_input(&v.node.value))
                    || Self::has_external_input(output)
            }
            Expression::Hold { body, .. } => Self::has_external_input(body),
            Expression::Latest { inputs } => inputs.iter().any(Self::has_external_input),
            Expression::Then { body } => Self::has_external_input(body),
            Expression::While { arms } => arms.iter().any(|a| Self::has_external_input(&a.body)),
            Expression::ArithmeticOperator(op) => match op {
                ArithmeticOperator::Add { operand_a, operand_b }
                | ArithmeticOperator::Subtract { operand_a, operand_b }
                | ArithmeticOperator::Multiply { operand_a, operand_b }
                | ArithmeticOperator::Divide { operand_a, operand_b } => {
                    Self::has_external_input(operand_a) || Self::has_external_input(operand_b)
                }
                ArithmeticOperator::Negate { operand } => Self::has_external_input(operand),
            },
            _ => false,
        }
    }

    fn is_reactive(expr: &Spanned<Expression>) -> bool {
        match &expr.node {
            Expression::Link | Expression::LinkSetter { .. } => true,
            Expression::Hold { .. } => true,
            Expression::Latest { .. } => true,
            Expression::Then { .. } => true,
            Expression::While { .. } => true,
            Expression::Variable(var) => Self::is_reactive(&var.value),
            Expression::Pipe { from, to } => Self::is_reactive(from) || Self::is_reactive(to),
            Expression::When { .. } => true,
            Expression::FunctionCall { path, arguments } => {
                let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                if matches!(path_strs.as_slice(),
                    ["Timer", "interval"] | ["Stream", "skip"] | ["Router", "route"] | ["Router", "go_to"]
                    | ["List", "count"] | ["List", "retain"] | ["List", "map"] | ["List", "append"] | ["List", "clear"] | ["List", "remove"]
                ) {
                    return true;
                }
                arguments.iter().any(|a| {
                    a.node
                        .value
                        .as_ref()
                        .map(|v| Self::is_reactive(v))
                        .unwrap_or(false)
                })
            }
            Expression::List { items } => items.iter().any(Self::is_reactive),
            Expression::Object(obj) => obj
                .variables
                .iter()
                .any(|v| Self::is_reactive(&v.node.value)),
            Expression::Block { variables, output } => {
                variables.iter().any(|v| Self::is_reactive(&v.node.value))
                    || Self::is_reactive(output)
            }
            Expression::ArithmeticOperator(op) => match op {
                ArithmeticOperator::Add {
                    operand_a,
                    operand_b,
                }
                | ArithmeticOperator::Subtract {
                    operand_a,
                    operand_b,
                }
                | ArithmeticOperator::Multiply {
                    operand_a,
                    operand_b,
                }
                | ArithmeticOperator::Divide {
                    operand_a,
                    operand_b,
                } => Self::is_reactive_or_alias(operand_a) || Self::is_reactive_or_alias(operand_b),
                ArithmeticOperator::Negate { operand } => Self::is_reactive_or_alias(operand),
            },
            Expression::Comparator(cmp) => {
                use static_expression::Comparator;
                let (a, b) = match cmp {
                    Comparator::Equal { operand_a, operand_b }
                    | Comparator::NotEqual { operand_a, operand_b }
                    | Comparator::Greater { operand_a, operand_b }
                    | Comparator::GreaterOrEqual { operand_a, operand_b }
                    | Comparator::Less { operand_a, operand_b }
                    | Comparator::LessOrEqual { operand_a, operand_b } => {
                        (operand_a, operand_b)
                    }
                };
                Self::is_reactive_or_alias(a) || Self::is_reactive_or_alias(b)
            }
            Expression::TextLiteral { parts } => parts.iter().any(|p| matches!(p, TextPart::Interpolation { .. })),
            Expression::Alias(_) => {
                // An alias might reference a reactive variable — conservative.
                // But for top-level reactivity check, we look at definitions.
                false
            }
            _ => false,
        }
    }

    /// Like `is_reactive`, but also returns true for aliases.
    /// Used for arithmetic/comparison operands where aliases likely reference
    /// sibling reactive vars (e.g., `todos_count - completed_todos_count`).
    fn is_reactive_or_alias(expr: &Spanned<Expression>) -> bool {
        matches!(&expr.node, Expression::Alias(_)) || Self::is_reactive(expr)
    }

    // -----------------------------------------------------------------------
    // Static evaluation (non-reactive programs)
    // -----------------------------------------------------------------------

    fn eval_static(&self, expr: &Spanned<Expression>) -> Result<Value, String> {
        self.eval_static_with_scope(expr, &IndexMap::new())
    }

    fn eval_static_with_scope(
        &self,
        expr: &Spanned<Expression>,
        local_scope: &IndexMap<String, Value>,
    ) -> Result<Value, String> {
        match &expr.node {
            Expression::Literal(lit) => Ok(Self::eval_literal(lit)),

            Expression::TextLiteral { parts } => {
                let mut result = String::new();
                for part in parts {
                    match part {
                        TextPart::Text(s) => result.push_str(s.as_str()),
                        TextPart::Interpolation { var, .. } => {
                            let val = self.resolve_alias_static(var.as_str(), local_scope)?;
                            result.push_str(&val.to_display_string());
                        }
                    }
                }
                Ok(Value::text(result))
            }

            Expression::FunctionCall { path, arguments } => {
                self.eval_function_call_static(path, arguments, local_scope)
            }

            Expression::Pipe { from, to } => self.eval_pipe_static(from, to, local_scope),

            Expression::Alias(alias) => match alias {
                Alias::WithoutPassed { parts, .. } => {
                    let name = parts[0].as_str();
                    let mut val = self.resolve_alias_static(name, local_scope)?;
                    // Follow field access: a.b.c
                    for part in &parts[1..] {
                        val = val
                            .get_field(part.as_str())
                            .cloned()
                            .ok_or_else(|| format!("Field '{}' not found on {}", part.as_str(), val))?;
                    }
                    Ok(val)
                }
                Alias::WithPassed { extra_parts } => {
                    // Look up __passed in local scope
                    let passed = local_scope.get("__passed")
                        .ok_or_else(|| "PASSED not available in this context".to_string())?;
                    let mut val = passed.clone();
                    for part in extra_parts {
                        val = val
                            .get_field(part.as_str())
                            .cloned()
                            .ok_or_else(|| format!("Field '{}' not found on PASSED", part.as_str()))?;
                    }
                    Ok(val)
                }
            },

            Expression::List { items } => {
                let mut fields = BTreeMap::new();
                for (i, item) in items.iter().enumerate() {
                    let val = self.eval_static_with_scope(item, local_scope)?;
                    fields.insert(Arc::from(format!("{:04}", i)), val);
                }
                Ok(Value::Tagged {
                    tag: Arc::from("List"),
                    fields: Arc::new(fields),
                })
            }

            Expression::Object(obj) => {
                let mut fields = BTreeMap::new();
                for var in &obj.variables {
                    let name = var.node.name.as_str().to_string();
                    let val = self.eval_static_with_scope(&var.node.value, local_scope)?;
                    fields.insert(Arc::from(name.as_str()), val);
                }
                Ok(Value::Object(Arc::new(fields)))
            }

            Expression::TaggedObject { tag, object } => {
                let mut fields = BTreeMap::new();
                for var in &object.variables {
                    let name = var.node.name.as_str().to_string();
                    let val = self.eval_static_with_scope(&var.node.value, local_scope)?;
                    fields.insert(Arc::from(name.as_str()), val);
                }
                Ok(Value::Tagged {
                    tag: Arc::from(tag.as_str()),
                    fields: Arc::new(fields),
                })
            }

            Expression::Block {
                variables, output, ..
            } => {
                let mut new_scope = local_scope.clone();
                for var in variables {
                    let val = self.eval_static_with_scope(&var.node.value, &new_scope)?;
                    new_scope.insert(var.node.name.as_str().to_string(), val);
                }
                self.eval_static_with_scope(output, &new_scope)
            }

            Expression::ArithmeticOperator(op) => self.eval_arithmetic_static(op, local_scope),

            Expression::Comparator(cmp) => self.eval_comparator_static(cmp, local_scope),

            Expression::When { .. } => {
                Err("WHEN requires pipe input in static context".to_string())
            }

            Expression::Skip => Ok(Value::Unit),

            // LINK in static context becomes a marker
            Expression::Link => Ok(Value::tag("LINK")),

            // LATEST in static context: evaluate each input, return last successful value
            Expression::Latest { inputs } => {
                let mut last_ok = None;
                for input in inputs {
                    if let Ok(val) = self.eval_static_with_scope(input, local_scope) {
                        if !matches!(val, Value::Unit) {
                            last_ok = Some(val);
                        }
                    }
                }
                last_ok.ok_or_else(|| "LATEST: no inputs resolved".to_string())
            }

            // Spread operator: evaluate inner and include it directly
            Expression::Spread { value } => {
                self.eval_static_with_scope(value, local_scope)
            }

            // HOLD in static context (non-piped): just return initial state
            Expression::Hold { .. } => {
                Err("HOLD requires pipe input in static context".to_string())
            }

            // THEN in static context (non-piped): evaluate body
            Expression::Then { body } => {
                self.eval_static_with_scope(body, local_scope)
            }

            _ => Err(format!("Unsupported expression in static eval: {:?}", std::mem::discriminant(&expr.node))),
        }
    }

    /// Evaluate an expression statically, but tolerate errors on object/tagged fields.
    /// Fields that can't be evaluated statically get `Value::Unit` as a default.
    /// This is used for the general program's initial state where some fields are
    /// purely reactive (event-driven) and can't be computed at compile time.
    fn eval_static_tolerant(
        &self,
        expr: &Spanned<Expression>,
        local_scope: &IndexMap<String, Value>,
    ) -> Value {
        match &expr.node {
            Expression::Object(obj) => {
                let mut fields = BTreeMap::new();
                for var in &obj.variables {
                    let name = var.node.name.as_str().to_string();
                    let val = self.eval_static_tolerant(&var.node.value, local_scope);
                    fields.insert(Arc::from(name.as_str()), val);
                }
                Value::Object(Arc::new(fields))
            }
            Expression::TaggedObject { tag, object } => {
                let mut fields = BTreeMap::new();
                for var in &object.variables {
                    let name = var.node.name.as_str().to_string();
                    let val = self.eval_static_tolerant(&var.node.value, local_scope);
                    fields.insert(Arc::from(name.as_str()), val);
                }
                Value::Tagged {
                    tag: Arc::from(tag.as_str()),
                    fields: Arc::new(fields),
                }
            }
            Expression::Block { variables, output, .. } => {
                let mut new_scope = local_scope.clone();
                for var in variables {
                    let val = self.eval_static_tolerant(&var.node.value, &new_scope);
                    new_scope.insert(var.node.name.as_str().to_string(), val);
                }
                self.eval_static_tolerant(output, &new_scope)
            }
            Expression::Pipe { from, to } => {
                // Handle HOLD specially: return initial value for reactive bodies
                if let Expression::Hold { state_param, body } = &to.node {
                    let initial_val = self.eval_static_tolerant(from, local_scope);
                    // Try the fold path; fall back to initial value
                    self.eval_hold_static(state_param.as_str(), &initial_val, body, local_scope)
                        .unwrap_or(initial_val)
                } else {
                    // Try strict eval first
                    if let Ok(val) = self.eval_pipe_static(from, to, local_scope) {
                        return val;
                    }
                    // Strict failed — try tolerant evaluation of the pipe.
                    // Evaluate 'from' tolerantly, then apply 'to' tolerantly.
                    let from_val = self.eval_static_tolerant(from, local_scope);
                    match &to.node {
                        Expression::While { arms } | Expression::When { arms } => {
                            // Try matching the from_val against WHILE/WHEN arms
                            for arm in arms {
                                if let Some(bindings) = self.match_pattern(&from_val, &arm.pattern) {
                                    let mut arm_scope = local_scope.clone();
                                    arm_scope.extend(bindings);
                                    return self.eval_static_tolerant(&arm.body, &arm_scope);
                                }
                            }
                            // No arm matched — return Unit for WHEN, from_val for WHILE
                            if matches!(&to.node, Expression::When { .. }) {
                                Value::Unit
                            } else {
                                from_val
                            }
                        }
                        Expression::Then { body } => {
                            self.eval_static_tolerant(body, local_scope)
                        }
                        Expression::FunctionCall { path, arguments } => {
                            // Re-evaluate pipe with tolerant from_val
                            // Build a temporary scope with the from_val
                            let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                            match path_strs.as_slice() {
                                ["List", "map"] => {
                                    let item_param = arguments.first()
                                        .map(|a| a.node.name.as_str().to_string())
                                        .unwrap_or_else(|| "item".to_string());
                                    let map_expr = arguments.iter()
                                        .find(|a| a.node.name.as_str() == "new")
                                        .and_then(|a| a.node.value.as_ref());
                                    if let Some(expr) = map_expr {
                                        let expr = expr.clone();
                                        from_val.list_map(|item_val| {
                                            let mut map_scope = local_scope.clone();
                                            map_scope.insert(item_param.clone(), item_val.clone());
                                            self.eval_static_tolerant(&expr, &map_scope)
                                        })
                                    } else {
                                        from_val
                                    }
                                }
                                ["List", "retain"] => {
                                    let item_param = arguments.first()
                                        .map(|a| a.node.name.as_str().to_string())
                                        .unwrap_or_else(|| "item".to_string());
                                    let if_expr = arguments.iter()
                                        .find(|a| a.node.name.as_str() == "if")
                                        .and_then(|a| a.node.value.as_ref());
                                    if let Some(pred) = if_expr {
                                        let pred = pred.clone();
                                        from_val.list_retain(|item_val| {
                                            let mut pred_scope = local_scope.clone();
                                            pred_scope.insert(item_param.clone(), item_val.clone());
                                            self.eval_static_tolerant(&pred, &pred_scope)
                                                .as_bool()
                                                .unwrap_or(true)
                                        })
                                    } else {
                                        from_val
                                    }
                                }
                                _ => from_val,
                            }
                        }
                        Expression::LinkSetter { alias } => {
                            // LINK binding — try to evaluate with from_val
                            self.eval_pipe_static(from, to, local_scope)
                                .unwrap_or(from_val)
                        }
                        Expression::FieldAccess { path: field_path } => {
                            let mut val = from_val;
                            for field in field_path {
                                val = val.get_field(field.as_str())
                                    .cloned()
                                    .unwrap_or(Value::Unit);
                            }
                            val
                        }
                        _ => from_val,
                    }
                }
            }
            Expression::List { items } => {
                let mut fields = BTreeMap::new();
                for (i, item) in items.iter().enumerate() {
                    let val = self.eval_static_tolerant(item, local_scope);
                    fields.insert(Arc::from(format!("{:04}", i)), val);
                }
                Value::Tagged {
                    tag: Arc::from("List"),
                    fields: Arc::new(fields),
                }
            }
            Expression::FunctionCall { path, arguments } => {
                // Try strict eval first
                if let Ok(val) = self.eval_function_call_static(path, arguments, local_scope) {
                    return val;
                }
                // Tolerant fallback based on function type
                let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                match path_strs.as_slice() {
                    // Element functions: evaluate arguments tolerantly
                    ["Element", kind] => {
                        let tag = match *kind {
                            "stripe" => "ElementStripe",
                            "stack" => "ElementStack",
                            "button" => "ElementButton",
                            "text_input" => "ElementTextInput",
                            "label" => "ElementLabel",
                            "link" => "ElementLink",
                            "checkbox" => "ElementCheckbox",
                            "paragraph" => "ElementParagraph",
                            "container" => "ElementContainer",
                            _ => return Value::Unit,
                        };
                        // Evaluate 'element' arg first for self-references (element.hovered etc.)
                        let mut elem_scope = local_scope.clone();
                        let mut hover_path: Option<String> = None;
                        if let Some(el_arg) = arguments.iter().find(|a| a.node.name.as_str() == "element") {
                            if let Some(ref val_expr) = el_arg.node.value {
                                let el_val = self.eval_static_tolerant(val_expr, local_scope);
                                // Check for hovered: LINK and resolve hover state from scope
                                let has_hovered_link = el_val.get_field("hovered")
                                    .map(|v| v.as_tag() == Some("LINK"))
                                    .unwrap_or(false);
                                if has_hovered_link {
                                    let hover_state = local_scope.values()
                                        .find_map(|v| v.get_field("__hovered"))
                                        .cloned()
                                        .unwrap_or(Value::tag("False"));
                                    hover_path = local_scope.values()
                                        .find_map(|v| v.get_field("__hover_path__").and_then(|p| p.as_text().map(|s| s.to_string())));
                                    let el_val = el_val.update_field("hovered", hover_state);
                                    let el_val = Self::replace_links_with_defaults(&el_val);
                                    elem_scope.insert("element".to_string(), el_val);
                                } else {
                                    let el_val = Self::replace_links_with_defaults(&el_val);
                                    elem_scope.insert("element".to_string(), el_val);
                                }
                            }
                        }
                        let mut fields = BTreeMap::new();
                        for arg in arguments {
                            let name = arg.node.name.as_str();
                            if let Some(ref val_expr) = arg.node.value {
                                let val = self.eval_static_tolerant(val_expr, &elem_scope);
                                fields.insert(Arc::from(name), val);
                            }
                        }
                        // Inject __hover_path__ for the bridge
                        if let Some(ref path) = hover_path {
                            fields.insert(Arc::from("__hover_path__"), Value::text(path.as_str()));
                        }
                        Value::Tagged {
                            tag: Arc::from(tag),
                            fields: Arc::new(fields),
                        }
                    }
                    ["Document", "new"] => {
                        let mut fields = BTreeMap::new();
                        for arg in arguments {
                            let name = arg.node.name.as_str();
                            if let Some(ref val_expr) = arg.node.value {
                                let val = self.eval_static_tolerant(val_expr, local_scope);
                                fields.insert(Arc::from(name), val);
                            }
                        }
                        Value::Tagged {
                            tag: Arc::from("DocumentNew"),
                            fields: Arc::new(fields),
                        }
                    }
                    // User-defined function: try tolerant body evaluation
                    [fn_name] => {
                        self.eval_user_function_tolerant(fn_name, arguments, local_scope)
                    }
                    _ => Value::Unit,
                }
            }
            // Alias: try strict eval with special handling for element self-references
            Expression::Alias(alias) => {
                if let Ok(val) = self.eval_static_with_scope(expr, local_scope) {
                    return val;
                }
                // Handle element self-references when element is not in scope
                match alias {
                    Alias::WithoutPassed { parts, .. }
                        if !parts.is_empty() && parts[0].as_str() == "element" =>
                    {
                        if parts.len() >= 2 && parts[1].as_str() == "hovered" {
                            Value::tag("False")
                        } else {
                            Value::Unit
                        }
                    }
                    _ => Value::Unit,
                }
            }
            // For anything else, try strict eval and fall back to Unit
            _ => self.eval_static_with_scope(expr, local_scope)
                .unwrap_or(Value::Unit),
        }
    }

    /// Like eval_user_function_static but uses tolerant evaluation for the body.
    fn eval_user_function_tolerant(
        &self,
        fn_name: &str,
        arguments: &[Spanned<Argument>],
        local_scope: &IndexMap<String, Value>,
    ) -> Value {
        let func = match self.functions.iter().find(|(name, _, _)| name == fn_name) {
            Some(f) => f.clone(),
            None => return Value::Unit,
        };

        let (_, params, body) = func;
        let mut fn_scope = local_scope.clone();

        for arg in arguments {
            let arg_name = arg.node.name.as_str();
            if arg_name == "PASS" {
                if let Some(ref val_expr) = arg.node.value {
                    let val = self.eval_static_tolerant(val_expr, local_scope);
                    fn_scope.insert("__passed".to_string(), val);
                }
                continue;
            }
            if let Some(ref val_expr) = arg.node.value {
                let val = self.eval_static_tolerant(val_expr, local_scope);
                fn_scope.insert(arg_name.to_string(), val);
            }
        }

        for (i, param_name) in params.iter().enumerate() {
            if i < arguments.len() {
                if let Some(ref val_expr) = arguments[i].node.value {
                    let val = self.eval_static_tolerant(val_expr, local_scope);
                    fn_scope.insert(param_name.clone(), val);
                }
            }
        }

        self.eval_static_tolerant(&body, &fn_scope)
    }

    fn eval_literal(lit: &Literal) -> Value {
        match lit {
            Literal::Number(n) => Value::number(*n),
            Literal::Tag(t) => Value::tag(t.as_str()),
            Literal::Text(s) => Value::text(s.as_str()),
        }
    }

    fn resolve_alias_static(
        &self,
        name: &str,
        local_scope: &IndexMap<String, Value>,
    ) -> Result<Value, String> {
        // Check local scope first
        if let Some(v) = local_scope.get(name) {
            return Ok(v.clone());
        }
        // Check top-level variables
        if let Some(expr) = self.get_var_expr(name) {
            return self.eval_static_with_scope(expr, local_scope);
        }
        // Uppercase identifiers are tags (True, False, Bold, Home, etc.)
        if name.chars().next().map_or(false, |c| c.is_uppercase()) {
            return Ok(Value::tag(name));
        }
        Err(format!("Variable '{}' not found", name))
    }

    fn eval_function_call_static(
        &self,
        path: &[crate::parser::StrSlice],
        arguments: &[Spanned<Argument>],
        local_scope: &IndexMap<String, Value>,
    ) -> Result<Value, String> {
        let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();

        match path_strs.as_slice() {
            ["Document", "new"] => {
                let mut fields = BTreeMap::new();
                for arg in arguments {
                    let name = arg.node.name.as_str();
                    if let Some(ref val_expr) = arg.node.value {
                        let val = self.eval_static_with_scope(val_expr, local_scope)?;
                        fields.insert(Arc::from(name), val);
                    }
                }
                Ok(Value::Tagged {
                    tag: Arc::from("DocumentNew"),
                    fields: Arc::new(fields),
                })
            }

            ["Element", "button"] => {
                self.eval_element_static("ElementButton", arguments, local_scope)
            }

            ["Element", "stripe"] => {
                self.eval_element_static("ElementStripe", arguments, local_scope)
            }

            ["Element", "container"] => {
                self.eval_element_static("ElementContainer", arguments, local_scope)
            }

            ["Element", "stack"] => {
                self.eval_element_static("ElementStack", arguments, local_scope)
            }

            ["Element", "paragraph"] => {
                self.eval_element_static("ElementParagraph", arguments, local_scope)
            }

            ["Element", "text_input"] => {
                self.eval_element_static("ElementTextInput", arguments, local_scope)
            }

            ["Element", "label"] => {
                self.eval_element_static("ElementLabel", arguments, local_scope)
            }

            ["Element", "link"] => {
                self.eval_element_static("ElementLink", arguments, local_scope)
            }

            ["Element", "checkbox"] => {
                self.eval_element_static("ElementCheckbox", arguments, local_scope)
            }

            ["Math", "sum"] => {
                // Static Math/sum — meaningless without reactive input
                Ok(Value::number(0.0))
            }

            // Text utilities
            ["Text", "empty"] => Ok(Value::text("")),
            ["Text", "space"] => Ok(Value::text(" ")),
            ["Text", "trim"] => {
                // Non-piped: Text/trim() with no args (shouldn't happen)
                Ok(Value::text(""))
            }
            ["Text", "is_not_empty"] => {
                // Non-piped: Text/is_not_empty() with no args
                Ok(Value::tag("False"))
            }
            ["Text", "is_empty"] => Ok(Value::tag("True")),

            // Bool utilities
            ["Bool", "not"] => {
                // Non-piped: shouldn't happen but handle it
                Ok(Value::tag("True"))
            }

            // List utilities (non-piped)
            ["List", "count"] => Ok(Value::number(0.0)),
            ["List", "is_empty"] => Ok(Value::tag("True")),

            [fn_name] => {
                // User-defined function call
                self.eval_user_function_static(fn_name, arguments, local_scope)
            }

            _ => Err(format!("Unknown function: {}", path_strs.join("/"))),
        }
    }

    fn eval_element_static(
        &self,
        tag: &str,
        arguments: &[Spanned<Argument>],
        local_scope: &IndexMap<String, Value>,
    ) -> Result<Value, String> {
        // Evaluate the 'element' argument first and add to scope,
        // so that self-references like `element.hovered` resolve.
        // LINK markers are replaced with defaults (hovered → False, events → Unit).
        let mut elem_scope = local_scope.clone();
        let mut hover_path: Option<String> = None;
        if let Some(el_arg) = arguments.iter().find(|a| a.node.name.as_str() == "element") {
            if let Some(ref val_expr) = el_arg.node.value {
                let el_val = self.eval_static_with_scope(val_expr, local_scope)?;
                // Check if element has hovered: LINK and resolve hover state from scope
                let has_hovered_link = el_val.get_field("hovered")
                    .map(|v| v.as_tag() == Some("LINK"))
                    .unwrap_or(false);
                if has_hovered_link {
                    // Look for __hovered state from any scope variable (from list item)
                    let hover_state = local_scope.values()
                        .find_map(|v| v.get_field("__hovered"))
                        .cloned()
                        .unwrap_or(Value::tag("False"));
                    // Look for __hover_path__ from any scope variable
                    hover_path = local_scope.values()
                        .find_map(|v| v.get_field("__hover_path__").and_then(|p| p.as_text().map(|s| s.to_string())));
                    // Replace hovered with actual hover state
                    let el_val = el_val.update_field("hovered", hover_state);
                    let el_val = Self::replace_links_with_defaults(&el_val);
                    elem_scope.insert("element".to_string(), el_val);
                } else {
                    let el_val = Self::replace_links_with_defaults(&el_val);
                    elem_scope.insert("element".to_string(), el_val);
                }
            }
        }
        let mut fields = BTreeMap::new();
        for arg in arguments {
            let name = arg.node.name.as_str();
            if let Some(ref val_expr) = arg.node.value {
                let val = self.eval_static_with_scope(val_expr, &elem_scope)?;
                fields.insert(Arc::from(name), val);
            }
        }
        // Inject __hover_path__ for the bridge to extract
        if let Some(ref path) = hover_path {
            fields.insert(Arc::from("__hover_path__"), Value::text(path.as_str()));
        }
        Ok(Value::Tagged {
            tag: Arc::from(tag),
            fields: Arc::new(fields),
        })
    }

    /// Replace LINK marker tags with sensible defaults for static evaluation.
    /// `hovered: LINK` → `hovered: False`, other LINKs → `False`.
    fn replace_links_with_defaults(val: &Value) -> Value {
        match val {
            Value::Tag(t) if t.as_ref() == "LINK" => Value::tag("False"),
            Value::Object(fields) => {
                let new_fields: BTreeMap<_, _> = fields
                    .iter()
                    .map(|(k, v)| (k.clone(), Self::replace_links_with_defaults(v)))
                    .collect();
                Value::Object(Arc::new(new_fields))
            }
            _ => val.clone(),
        }
    }

    fn eval_user_function_static(
        &self,
        fn_name: &str,
        arguments: &[Spanned<Argument>],
        local_scope: &IndexMap<String, Value>,
    ) -> Result<Value, String> {
        let func = self
            .functions
            .iter()
            .find(|(name, _, _)| name == fn_name)
            .ok_or_else(|| format!("Function '{}' not found", fn_name))?
            .clone();

        let (_, params, body) = func;

        let mut fn_scope = local_scope.clone();

        // Handle PASS argument — sets __passed context for the function
        for arg in arguments {
            let arg_name = arg.node.name.as_str();
            if arg_name == "PASS" {
                if let Some(ref val_expr) = arg.node.value {
                    let val = self.eval_static_with_scope(val_expr, local_scope)?;
                    fn_scope.insert("__passed".to_string(), val);
                }
                continue;
            }
            if let Some(ref val_expr) = arg.node.value {
                let val = self.eval_static_with_scope(val_expr, local_scope)?;
                fn_scope.insert(arg_name.to_string(), val);
            }
        }

        for (i, param_name) in params.iter().enumerate() {
            if i < arguments.len() {
                if let Some(ref val_expr) = arguments[i].node.value {
                    let val = self.eval_static_with_scope(val_expr, local_scope)?;
                    fn_scope.insert(param_name.clone(), val);
                }
            }
        }

        self.eval_static_with_scope(&body, &fn_scope)
    }

    fn eval_pipe_static(
        &self,
        from: &Spanned<Expression>,
        to: &Spanned<Expression>,
        local_scope: &IndexMap<String, Value>,
    ) -> Result<Value, String> {
        // Handle HOLD specially — needs access to unevaluated body for fold
        if let Expression::Hold { state_param, body } = &to.node {
            let initial_val = self.eval_static_with_scope(from, local_scope)?;
            return self.eval_hold_static(state_param.as_str(), &initial_val, body, local_scope);
        }

        let from_val = self.eval_static_with_scope(from, local_scope)?;

        match &to.node {
            Expression::FunctionCall { path, arguments } => {
                let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                match path_strs.as_slice() {
                    ["Document", "new"] => {
                        if arguments.is_empty() {
                            Ok(Value::tagged("DocumentNew", [("root", from_val)]))
                        } else {
                            self.eval_function_call_static(path, arguments, local_scope)
                        }
                    }
                    ["Stream", "skip"] => {
                        let count = arguments.iter()
                            .find(|a| a.node.name.as_str() == "count")
                            .and_then(|a| a.node.value.as_ref())
                            .and_then(|v| self.eval_static_with_scope(v, local_scope).ok())
                            .and_then(|v| v.as_number())
                            .unwrap_or(0.0) as usize;

                        if let Value::Tagged { tag, fields } = &from_val {
                            if tag.as_ref() == "List" {
                                let items: Vec<&Value> = fields.values().collect();
                                if count < items.len() {
                                    return Ok(items[count].clone());
                                }
                            }
                        }
                        if count == 0 {
                            Ok(from_val)
                        } else {
                            Err("Stream/skip: not enough values".to_string())
                        }
                    }
                    ["Stream", "pulses"] => {
                        let n = from_val.as_number().unwrap_or(0.0) as usize;
                        let mut fields = BTreeMap::new();
                        for i in 0..n {
                            fields.insert(Arc::from(format!("{:04}", i)), Value::Unit);
                        }
                        Ok(Value::Tagged {
                            tag: Arc::from("List"),
                            fields: Arc::new(fields),
                        })
                    }
                    ["Log", "info"] => {
                        Ok(from_val)
                    }

                    // Text utilities (piped)
                    ["Text", "trim"] => {
                        let s = from_val.as_text().unwrap_or("");
                        Ok(Value::text(s.trim()))
                    }
                    ["Text", "is_not_empty"] => {
                        let s = from_val.as_text().unwrap_or("");
                        Ok(if !s.is_empty() { Value::tag("True") } else { Value::tag("False") })
                    }
                    ["Text", "is_empty"] => {
                        let s = from_val.as_text().unwrap_or("");
                        Ok(if s.is_empty() { Value::tag("True") } else { Value::tag("False") })
                    }

                    // Bool utilities (piped)
                    ["Bool", "not"] => {
                        let b = from_val.as_bool().unwrap_or(false);
                        Ok(if b { Value::tag("False") } else { Value::tag("True") })
                    }
                    ["Bool", "or"] => {
                        let a = from_val.as_bool().unwrap_or(false);
                        let b = arguments.iter()
                            .find(|arg| arg.node.name.as_str() == "that")
                            .and_then(|arg| arg.node.value.as_ref())
                            .and_then(|v| self.eval_static_with_scope(v, local_scope).ok())
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        Ok(if a || b { Value::tag("True") } else { Value::tag("False") })
                    }
                    ["Bool", "and"] => {
                        let a = from_val.as_bool().unwrap_or(false);
                        let b = arguments.iter()
                            .find(|arg| arg.node.name.as_str() == "that")
                            .and_then(|arg| arg.node.value.as_ref())
                            .and_then(|v| self.eval_static_with_scope(v, local_scope).ok())
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        Ok(if a && b { Value::tag("True") } else { Value::tag("False") })
                    }

                    // List utilities (piped)
                    ["List", "count"] => {
                        Ok(Value::number(from_val.list_count() as f64))
                    }
                    ["List", "is_empty"] => {
                        Ok(if from_val.list_is_empty() { Value::tag("True") } else { Value::tag("False") })
                    }
                    ["List", "map"] => {
                        // `list |> List/map(item, new: expr)`
                        // Extract the item parameter name and mapping expression
                        let item_param = arguments.first()
                            .map(|a| a.node.name.as_str().to_string())
                            .unwrap_or_else(|| "item".to_string());
                        let map_expr = arguments.iter()
                            .find(|a| a.node.name.as_str() == "new")
                            .and_then(|a| a.node.value.as_ref());
                        if let Some(expr) = map_expr {
                            let expr = expr.clone();
                            let mapped = from_val.list_map(|item_val| {
                                let mut map_scope = local_scope.clone();
                                map_scope.insert(item_param.clone(), item_val.clone());
                                // Use tolerant eval — mapping functions often contain
                                // reactive elements (WHILE, WHEN, etc.) that need tolerance
                                self.eval_static_tolerant(&expr, &map_scope)
                            });
                            Ok(mapped)
                        } else {
                            Ok(from_val)
                        }
                    }
                    ["List", "append"] | ["List", "clear"] | ["List", "remove"] => {
                        // List mutation operations in static context — pass through
                        // (handled in general program transform)
                        Ok(from_val)
                    }
                    ["List", "retain"] => {
                        // `list |> List/retain(item, if: predicate_expr)`
                        let item_param = arguments.first()
                            .map(|a| a.node.name.as_str().to_string())
                            .unwrap_or_else(|| "item".to_string());
                        let if_expr = arguments.iter()
                            .find(|a| a.node.name.as_str() == "if")
                            .and_then(|a| a.node.value.as_ref());
                        if let Some(pred) = if_expr {
                            let pred = pred.clone();
                            let retained = from_val.list_retain(|item_val| {
                                let mut pred_scope = local_scope.clone();
                                pred_scope.insert(item_param.clone(), item_val.clone());
                                self.eval_static_tolerant(&pred, &pred_scope)
                                    .as_bool()
                                    .unwrap_or(true) // keep items if predicate fails
                            });
                            Ok(retained)
                        } else {
                            Ok(from_val)
                        }
                    }

                    [fn_name] => {
                        // Piped user function call: `value |> fn()` → fn(value)
                        if let Some(func) = self.functions.iter().find(|(n, _, _)| n == fn_name) {
                            let (_, params, body) = func.clone();
                            let mut fn_scope = local_scope.clone();
                            // Bind piped value to first parameter
                            if let Some(first_param) = params.first() {
                                fn_scope.insert(first_param.clone(), from_val.clone());
                            }
                            // Also bind named arguments
                            for arg in arguments {
                                let arg_name = arg.node.name.as_str();
                                if let Some(ref val_expr) = arg.node.value {
                                    let val = self.eval_static_with_scope(val_expr, local_scope)?;
                                    fn_scope.insert(arg_name.to_string(), val);
                                }
                            }
                            return self.eval_static_with_scope(&body, &fn_scope);
                        }
                        Err(format!("Unknown piped function: {}", fn_name))
                    }
                    _ => {
                        self.eval_function_call_static(path, arguments, local_scope)
                    }
                }
            }
            Expression::FieldAccess { path: field_path } => {
                let mut val = from_val;
                for field in field_path {
                    val = val
                        .get_field(field.as_str())
                        .cloned()
                        .ok_or_else(|| format!("Field '{}' not found", field.as_str()))?;
                }
                Ok(val)
            }
            Expression::While { arms } => {
                self.eval_while_static(&from_val, arms, local_scope)
            }
            Expression::When { arms } => {
                self.eval_when_static(&from_val, arms, local_scope)
            }
            Expression::Then { body } => {
                // THEN in static context: evaluate body for each event
                // In a fold context, this just applies the transform
                self.eval_static_with_scope(body, local_scope)
            }
            Expression::LinkSetter { alias } => {
                // LINK binding: add press_link and __link_path__ fields
                let link_path = match &alias.node {
                    Alias::WithoutPassed { parts, .. } => {
                        // Try to resolve the alias through scope to find __link_path__
                        // (e.g., `todo.todo_elements.todo_checkbox` where `todo` is in scope
                        //  with injected link paths)
                        let resolved = if !parts.is_empty() {
                            let first = parts[0].as_str();
                            if let Some(root_val) = local_scope.get(first) {
                                let mut val = root_val.clone();
                                for part in &parts[1..] {
                                    val = val.get_field(part.as_str())
                                        .cloned()
                                        .unwrap_or(Value::Unit);
                                }
                                val.get_field("__link_path__")
                                    .and_then(|v| v.as_text())
                                    .map(|s| s.to_string())
                            } else {
                                None
                            }
                        } else {
                            None
                        };
                        resolved.unwrap_or_else(|| {
                            parts.iter().map(|p| p.as_str()).collect::<Vec<_>>().join(".")
                        })
                    }
                    Alias::WithPassed { extra_parts } => {
                        // LINK { PASSED.store.elements.item_input }
                        // Resolve PASSED to get the actual link path
                        let passed = local_scope.get("__passed");
                        if let Some(passed_val) = passed {
                            let mut val = passed_val.clone();
                            for part in extra_parts {
                                val = val.get_field(part.as_str())
                                    .cloned()
                                    .unwrap_or(Value::Unit);
                            }
                            // The resolved value might be an object with __link_path__
                            val.get_field("__link_path__")
                                .and_then(|v| v.as_text())
                                .map(|s| s.to_string())
                                .or_else(|| val.as_text().map(|s| s.to_string()))
                                .unwrap_or_else(|| {
                                    // Build path from extra_parts
                                    extra_parts.iter().map(|p| p.as_str()).collect::<Vec<_>>().join(".")
                                })
                        } else {
                            extra_parts.iter().map(|p| p.as_str()).collect::<Vec<_>>().join(".")
                        }
                    }
                };
                let press_path = format!("{}.event.press", link_path);
                if let Value::Tagged { tag, fields } = from_val {
                    let mut new_fields = (*fields).clone();
                    new_fields.insert(Arc::from("press_link"), Value::text(&*press_path));
                    new_fields.insert(Arc::from("__link_path__"), Value::text(link_path.as_str()));
                    Ok(Value::Tagged { tag, fields: Arc::new(new_fields) })
                } else {
                    Ok(from_val)
                }
            }
            _ => {
                self.eval_static_with_scope(to, local_scope)
            }
        }
    }

    /// Evaluate HOLD statically as a fold operation.
    /// `initial |> HOLD state { body }` where body contains `N |> Stream/pulses() |> THEN { transform }`
    fn eval_hold_static(
        &self,
        state_name: &str,
        initial_val: &Value,
        body: &Spanned<Expression>,
        local_scope: &IndexMap<String, Value>,
    ) -> Result<Value, String> {
        // Extract the pattern: event_source |> THEN { transform }
        // where event_source is N |> Stream/pulses()
        match &body.node {
            Expression::Pipe { from, to } => {
                match &to.node {
                    Expression::Then { body: then_body } => {
                        // Try to evaluate as a fold (Stream/pulses pattern)
                        match self.eval_pulse_count(from, local_scope) {
                            Ok(pulse_count) => {
                                // Run the fold: apply then_body pulse_count times
                                let mut state = initial_val.clone();
                                let mut all_states = Vec::new();
                                all_states.push(state.clone());
                                for _ in 0..pulse_count {
                                    let mut fold_scope = local_scope.clone();
                                    fold_scope.insert(state_name.to_string(), state.clone());
                                    state = self.eval_static_with_scope(then_body, &fold_scope)?;
                                    all_states.push(state.clone());
                                }

                                // Return the sequence of all states as a List
                                let mut fields = BTreeMap::new();
                                for (i, s) in all_states.iter().enumerate() {
                                    fields.insert(Arc::from(format!("{:04}", i)), s.clone());
                                }
                                Ok(Value::Tagged {
                                    tag: Arc::from("List"),
                                    fields: Arc::new(fields),
                                })
                            }
                            Err(_) => {
                                // Not a Stream/pulses source (e.g., LATEST |> THEN) —
                                // reactive pattern, return initial value
                                Ok(initial_val.clone())
                            }
                        }
                    }
                    _ => {
                        // Reactive HOLD body (LATEST, WHEN, etc.) — return initial value.
                        // The body only matters at runtime for event processing.
                        Ok(initial_val.clone())
                    }
                }
            }
            _ => {
                // Non-pipe HOLD body (e.g., LATEST { ... }) — return initial value.
                Ok(initial_val.clone())
            }
        }
    }

    /// Extract the pulse count from an expression like `N |> Stream/pulses()` or `expr |> Stream/pulses()`.
    fn eval_pulse_count(
        &self,
        expr: &Spanned<Expression>,
        local_scope: &IndexMap<String, Value>,
    ) -> Result<usize, String> {
        match &expr.node {
            Expression::Pipe { from, to } => {
                if let Expression::FunctionCall { path, .. } = &to.node {
                    let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                    if path_strs.as_slice() == ["Stream", "pulses"] {
                        let val = self.eval_static_with_scope(from, local_scope)?;
                        return val.as_number()
                            .map(|n| n as usize)
                            .ok_or_else(|| "Stream/pulses count must be a number".to_string());
                    }
                }
                // Recurse for nested pipes
                let val = self.eval_static_with_scope(expr, local_scope)?;
                val.as_number()
                    .map(|n| n as usize)
                    .ok_or_else(|| "Expected number for pulse count".to_string())
            }
            _ => {
                let val = self.eval_static_with_scope(expr, local_scope)?;
                val.as_number()
                    .map(|n| n as usize)
                    .ok_or_else(|| "Expected number for pulse count".to_string())
            }
        }
    }

    /// Evaluate WHILE pattern matching statically.
    fn eval_while_static(
        &self,
        input: &Value,
        arms: &[static_expression::Arm],
        local_scope: &IndexMap<String, Value>,
    ) -> Result<Value, String> {
        for arm in arms {
            if let Some(bindings) = self.match_pattern(input, &arm.pattern) {
                let mut arm_scope = local_scope.clone();
                arm_scope.extend(bindings);
                return self.eval_static_with_scope(&arm.body, &arm_scope);
            }
        }
        Err(format!("No WHILE arm matched value: {}", input.to_display_string()))
    }

    /// Evaluate WHEN pattern matching statically.
    fn eval_when_static(
        &self,
        input: &Value,
        arms: &[static_expression::Arm],
        local_scope: &IndexMap<String, Value>,
    ) -> Result<Value, String> {
        for arm in arms {
            if let Some(bindings) = self.match_pattern(input, &arm.pattern) {
                let mut arm_scope = local_scope.clone();
                arm_scope.extend(bindings);
                return self.eval_static_with_scope(&arm.body, &arm_scope);
            }
        }
        // WHEN with no match → SKIP
        Ok(Value::Unit)
    }

    /// Match a value against a pattern, returning variable bindings if successful.
    fn match_pattern(
        &self,
        value: &Value,
        pattern: &static_expression::Pattern,
    ) -> Option<IndexMap<String, Value>> {
        use static_expression::Pattern;
        match pattern {
            Pattern::Literal(lit) => {
                let pat_val = Self::eval_literal(lit);
                if *value == pat_val {
                    Some(IndexMap::new())
                } else {
                    None
                }
            }
            Pattern::Alias { name } => {
                let name_str = name.as_str();
                if name_str == "__" || name_str == "_" {
                    // Wildcard
                    Some(IndexMap::new())
                } else if name_str.chars().next().map_or(false, |c| c.is_uppercase()) {
                    // Uppercase alias = tag comparison (True, False, Home, etc.)
                    let tag_val = Value::tag(name_str);
                    if *value == tag_val {
                        Some(IndexMap::new())
                    } else {
                        None
                    }
                } else {
                    // Lowercase alias = variable binding
                    let mut bindings = IndexMap::new();
                    bindings.insert(name_str.to_string(), value.clone());
                    Some(bindings)
                }
            }
            Pattern::TaggedObject { tag, variables } => {
                if let Value::Tagged { tag: val_tag, fields: val_fields } = value {
                    if tag.as_str() == val_tag.as_ref() {
                        let mut bindings = IndexMap::new();
                        for var in variables {
                            let field_name = var.name.as_str();
                            if let Some(field_val) = val_fields.get(field_name) {
                                if let Some(sub_pat) = &var.value {
                                    if let Some(sub_bindings) = self.match_pattern(field_val, sub_pat) {
                                        bindings.extend(sub_bindings);
                                    } else {
                                        return None;
                                    }
                                } else {
                                    bindings.insert(field_name.to_string(), field_val.clone());
                                }
                            } else {
                                return None;
                            }
                        }
                        return Some(bindings);
                    }
                }
                // Tag-only match (no fields)
                if variables.is_empty() {
                    if let Value::Tagged { tag: val_tag, .. } = value {
                        if tag.as_str() == val_tag.as_ref() {
                            return Some(IndexMap::new());
                        }
                    }
                }
                None
            }
            Pattern::Object { variables } => {
                let mut bindings = IndexMap::new();
                for var in variables {
                    let field_name = var.name.as_str();
                    if let Some(field_val) = value.get_field(field_name) {
                        bindings.insert(field_name.to_string(), field_val.clone());
                    } else {
                        return None;
                    }
                }
                Some(bindings)
            }
            Pattern::WildCard => {
                Some(IndexMap::new())
            }
            _ => {
                // Unsupported pattern — no match
                None
            }
        }
    }

    fn eval_arithmetic_static(
        &self,
        op: &ArithmeticOperator,
        local_scope: &IndexMap<String, Value>,
    ) -> Result<Value, String> {
        match op {
            ArithmeticOperator::Add {
                operand_a,
                operand_b,
            } => {
                let a = self.eval_static_with_scope(operand_a, local_scope)?;
                let b = self.eval_static_with_scope(operand_b, local_scope)?;
                Ok(Value::number(
                    a.as_number().unwrap_or(0.0) + b.as_number().unwrap_or(0.0),
                ))
            }
            ArithmeticOperator::Subtract {
                operand_a,
                operand_b,
            } => {
                let a = self.eval_static_with_scope(operand_a, local_scope)?;
                let b = self.eval_static_with_scope(operand_b, local_scope)?;
                Ok(Value::number(
                    a.as_number().unwrap_or(0.0) - b.as_number().unwrap_or(0.0),
                ))
            }
            ArithmeticOperator::Multiply {
                operand_a,
                operand_b,
            } => {
                let a = self.eval_static_with_scope(operand_a, local_scope)?;
                let b = self.eval_static_with_scope(operand_b, local_scope)?;
                Ok(Value::number(
                    a.as_number().unwrap_or(0.0) * b.as_number().unwrap_or(0.0),
                ))
            }
            ArithmeticOperator::Divide {
                operand_a,
                operand_b,
            } => {
                let a = self.eval_static_with_scope(operand_a, local_scope)?;
                let b = self.eval_static_with_scope(operand_b, local_scope)?;
                let bv = b.as_number().unwrap_or(1.0);
                if bv == 0.0 {
                    Ok(Value::number(f64::NAN))
                } else {
                    Ok(Value::number(a.as_number().unwrap_or(0.0) / bv))
                }
            }
            ArithmeticOperator::Negate { operand } => {
                let v = self.eval_static_with_scope(operand, local_scope)?;
                Ok(Value::number(-v.as_number().unwrap_or(0.0)))
            }
        }
    }

    fn eval_comparator_static(
        &self,
        cmp: &static_expression::Comparator,
        local_scope: &IndexMap<String, Value>,
    ) -> Result<Value, String> {
        use static_expression::Comparator;
        match cmp {
            Comparator::Equal {
                operand_a,
                operand_b,
            } => {
                let a = self.eval_static_with_scope(operand_a, local_scope)?;
                let b = self.eval_static_with_scope(operand_b, local_scope)?;
                // Use Tag("True")/Tag("False") to match Boon's pattern matching
                Ok(if a == b { Value::tag("True") } else { Value::tag("False") })
            }
            Comparator::NotEqual {
                operand_a,
                operand_b,
            } => {
                let a = self.eval_static_with_scope(operand_a, local_scope)?;
                let b = self.eval_static_with_scope(operand_b, local_scope)?;
                Ok(if a != b { Value::tag("True") } else { Value::tag("False") })
            }
            Comparator::Greater {
                operand_a,
                operand_b,
            } => {
                let a = self.eval_static_with_scope(operand_a, local_scope)?;
                let b = self.eval_static_with_scope(operand_b, local_scope)?;
                Ok(if a > b { Value::tag("True") } else { Value::tag("False") })
            }
            Comparator::GreaterOrEqual {
                operand_a,
                operand_b,
            } => {
                let a = self.eval_static_with_scope(operand_a, local_scope)?;
                let b = self.eval_static_with_scope(operand_b, local_scope)?;
                Ok(if a >= b { Value::tag("True") } else { Value::tag("False") })
            }
            Comparator::Less {
                operand_a,
                operand_b,
            } => {
                let a = self.eval_static_with_scope(operand_a, local_scope)?;
                let b = self.eval_static_with_scope(operand_b, local_scope)?;
                Ok(if a < b { Value::tag("True") } else { Value::tag("False") })
            }
            Comparator::LessOrEqual {
                operand_a,
                operand_b,
            } => {
                let a = self.eval_static_with_scope(operand_a, local_scope)?;
                let b = self.eval_static_with_scope(operand_b, local_scope)?;
                Ok(if a <= b { Value::tag("True") } else { Value::tag("False") })
            }
        }
    }

    // -----------------------------------------------------------------------
    // Reactive compilation → DataflowGraph
    // -----------------------------------------------------------------------

    fn compile_to_graph(
        &self,
        doc_expr: &Spanned<Expression>,
        storage_key: Option<&str>,
        persisted_holds: &std::collections::HashMap<String, Value>,
    ) -> Result<DataflowGraph, String> {
        // Try the real DD compilation path first.
        // If it fails (e.g., unsupported pattern), fall back to the general path.
        let mut ctx = GraphBuilder::new(self, storage_key, persisted_holds);
        ctx.compile_program(doc_expr)
    }
}

// ---------------------------------------------------------------------------
// Display pipeline info extracted from function bodies
// ---------------------------------------------------------------------------

/// Information about a display pipeline found in a function body.
/// Holds the retain arguments and map transform expression.
struct DisplayPipelineInfo {
    /// The arguments of the List/retain call (None if no retain, e.g., shopping_list).
    retain_arguments: Option<Vec<Spanned<Argument>>>,
    /// The item parameter name from List/map (e.g., "item").
    map_item_param: String,
    /// The `new:` expression from List/map (e.g., `todo_item(todo: item)`).
    map_new_expr: Spanned<Expression>,
}

// ---------------------------------------------------------------------------
// GraphBuilder — builds DataflowGraph from AST
// ---------------------------------------------------------------------------

struct GraphBuilder<'a> {
    compiler: &'a Compiler,
    collections: IndexMap<VarId, CollectionSpec>,
    inputs: Vec<InputSpec>,
    next_input_id: usize,
    next_anon_id: usize,
    /// Tracks which variables are reactive (have been added as collections).
    reactive_vars: IndexMap<String, VarId>,
    /// Tracks keyed hold variables (list name → keyed VarId).
    /// Keyed vars hold `(ListKey, Value)` pairs from KeyedHoldState.
    /// Used by compile_list_count and compile_list_retain to use DD-native
    /// keyed operators (O(1) per item) instead of scalar Map on assembled list.
    keyed_hold_vars: IndexMap<String, VarId>,
    /// Tracks VarIds that contain keyed `(ListKey, Value)` collections.
    /// Populated from keyed_hold_vars and extended by keyed operations
    /// (ListRetain, ListRetainReactive, ListMap, ListMapWithKey).
    /// Used by resolve_keyed_source to find keyed results of chained operations.
    keyed_collection_vars: HashSet<VarId>,
    /// Scope prefix for resolving nested object references.
    /// E.g., when compiling `store.nav_action`, scope_prefix = "store"
    /// so `nav.home.event.press` resolves to `store.nav.home.event.press`.
    scope_prefix: Option<String>,
    /// Storage key for localStorage persistence.
    storage_key: Option<String>,
    /// Pre-loaded persisted hold values (hold_name → value).
    persisted_holds: &'a std::collections::HashMap<String, Value>,
    /// Short name of the keyed list whose display pipeline was extracted
    /// into DD operators. When set, `build_doc_template` will emit an
    /// empty list for `items:` expressions that reference this list,
    /// so the bridge can populate items via keyed diffs instead.
    keyed_display_list_name: Option<String>,
}

impl<'a> GraphBuilder<'a> {
    fn new(
        compiler: &'a Compiler,
        storage_key: Option<&str>,
        persisted_holds: &'a std::collections::HashMap<String, Value>,
    ) -> Self {
        Self {
            compiler,
            collections: IndexMap::new(),
            inputs: Vec::new(),
            next_input_id: 0,
            next_anon_id: 0,
            reactive_vars: IndexMap::new(),
            keyed_hold_vars: IndexMap::new(),
            keyed_collection_vars: HashSet::new(),
            scope_prefix: None,
            storage_key: storage_key.map(|s| s.to_string()),
            persisted_holds,
            keyed_display_list_name: None,
        }
    }

    fn fresh_var(&mut self, prefix: &str) -> VarId {
        let id = self.next_anon_id;
        self.next_anon_id += 1;
        VarId::new(format!("{}_{}", prefix, id))
    }

    fn add_input(&mut self, kind: InputKind, link_path: Option<String>) -> InputId {
        self.add_input_with_timer(kind, link_path, None)
    }

    fn add_input_with_timer(
        &mut self,
        kind: InputKind,
        link_path: Option<String>,
        timer_interval_secs: Option<f64>,
    ) -> InputId {
        let id = InputId(self.next_input_id);
        self.next_input_id += 1;
        self.inputs.push(InputSpec {
            id,
            kind,
            link_path,
            timer_interval_secs,
        });
        id
    }

    fn compile_program(
        &mut self,
        doc_expr: &Spanned<Expression>,
    ) -> Result<DataflowGraph, String> {
        // Pass 1: Find all reactive variables and compile them
        let vars: Vec<(String, Spanned<Expression>)> = self.compiler.variables.clone();
        for (name, expr) in &vars {
            if name == "document" {
                continue; // Handle document separately
            }
            if Compiler::is_reactive(expr) {
                // Derive scope prefix from dotted variable names
                // e.g., "store.nav_action" → scope_prefix = "store"
                if let Some(dot_pos) = name.rfind('.') {
                    self.scope_prefix = Some(name[..dot_pos].to_string());
                } else {
                    self.scope_prefix = None;
                }
                self.compile_reactive_var(name, expr)?;
            }
        }
        self.scope_prefix = None;

        // Between Pass 1 and Pass 2: Build keyed display pipeline.
        // Scans function bodies for `PASSED.store.<keyed_name> |> List/retain(...) |> List/map(...)`
        // and compiles keyed DD operators: ListRetainReactive → ListMapWithKey.
        // The display_var points to the post-retain-post-map keyed collection for O(1) per-item diffs.
        let keyed_list_output = if let Some((list_name, keyed_var)) = self.keyed_hold_vars.first() {
            let list_name = list_name.clone();
            let keyed_var = keyed_var.clone();
            let short_name = list_name.strip_prefix("store.").unwrap_or(&list_name).to_string();
            let display_var = self.build_display_pipeline(&list_name, &keyed_var)
                .unwrap_or_else(|_| keyed_var.clone());
            // Find the element tag of the Stripe that displays keyed items
            let element_tag = self.compiler.find_keyed_stripe_element_tag(&short_name);
            // Record the list name so build_doc_template can emit empty items
            self.keyed_display_list_name = Some(short_name);
            Some(KeyedListOutput {
                display_var,
                persistence_var: keyed_var,
                // Persistence uses a separate inspect on persistence_var (raw data).
                // display_var inspect sends element Values to bridge only.
                storage_key: self.storage_key.clone(),
                hold_name: Some(list_name),
                element_tag,
            })
        } else {
            None
        };

        // Pass 2: Compile the document expression
        let doc_var = self.compile_document_expr(doc_expr)?;

        Ok(DataflowGraph {
            inputs: std::mem::take(&mut self.inputs),
            collections: std::mem::take(&mut self.collections),
            document: doc_var,
            storage_key: self.storage_key.clone(),
            keyed_list_output,
        })
    }

    fn compile_reactive_var(
        &mut self,
        name: &str,
        expr: &Spanned<Expression>,
    ) -> Result<VarId, String> {
        match &expr.node {
            // Alias references to existing reactive vars
            Expression::Alias(Alias::WithoutPassed { parts, .. }) => {
                let path = parts.iter().map(|p| p.as_str()).collect::<Vec<_>>().join(".");
                // Check with scope prefix first
                if let Some(ref prefix) = self.scope_prefix {
                    let prefixed = format!("{}.{}", prefix, path);
                    if let Some(var_id) = self.reactive_vars.get(&prefixed) {
                        return Ok(var_id.clone());
                    }
                }
                if let Some(var_id) = self.reactive_vars.get(&path) {
                    return Ok(var_id.clone());
                }
                // Not a reactive var — try to evaluate statically and create a Literal
                match self.compiler.eval_static(expr) {
                    Ok(value) => {
                        let var = self.fresh_var(&format!("{}_literal", name));
                        self.collections.insert(var.clone(), CollectionSpec::Literal(value));
                        Ok(var)
                    }
                    Err(e) => Err(format!("Alias '{}' is not a reactive var and cannot be evaluated statically: {}", path, e))
                }
            }

            Expression::Pipe { from, to } => {
                // Unwrap pipe chains to find the core reactive pattern
                self.compile_reactive_pipe(name, from, to)
            }

            // Pattern: `LATEST { ... }`
            Expression::Latest { inputs } => {
                self.compile_latest(name, inputs)
            }

            // Pattern: `Router/route()`
            Expression::FunctionCall { path, .. }
                if {
                    let p: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                    p.as_slice() == ["Router", "route"]
                } =>
            {
                let input_id = self.add_input(InputKind::Router, Some("__router".to_string()));
                let var = VarId::new(name);
                self.collections.insert(var.clone(), CollectionSpec::Input(input_id));
                self.reactive_vars.insert(name.to_string(), var.clone());
                Ok(var)
            }

            // Pattern: `reactive_a - reactive_b` (or +, *, /)
            Expression::ArithmeticOperator(op) => {
                self.compile_reactive_arithmetic(name, op)
            }

            // Pattern: `reactive_a == reactive_b` (or !=, <, >, <=, >=)
            Expression::Comparator(cmp) => {
                self.compile_reactive_comparison(name, cmp)
            }

            _ => {
                // Check if this is an element with LINK bindings
                if self.compiler.expr_contains_link(expr) {
                    // Element definitions are static but contain LINK markers
                    // Don't add to reactive collections
                    Ok(VarId::new(name))
                } else {
                    Err(format!("Unsupported reactive pattern for '{}': {:?}",
                        name, std::mem::discriminant(&expr.node)))
                }
            }
        }
    }

    /// Compile a reactive arithmetic operation (e.g., `todos_count - completed_todos_count`).
    fn compile_reactive_arithmetic(
        &mut self,
        name: &str,
        op: &ArithmeticOperator,
    ) -> Result<VarId, String> {
        let (operand_a, operand_b, op_fn): (_, _, Arc<dyn Fn(&Value, &Value) -> Value + 'static>) = match op {
            ArithmeticOperator::Add { operand_a, operand_b } => {
                (operand_a, operand_b, Arc::new(|a: &Value, b: &Value| {
                    Value::number(a.as_number().unwrap_or(0.0) + b.as_number().unwrap_or(0.0))
                }))
            }
            ArithmeticOperator::Subtract { operand_a, operand_b } => {
                (operand_a, operand_b, Arc::new(|a: &Value, b: &Value| {
                    Value::number(a.as_number().unwrap_or(0.0) - b.as_number().unwrap_or(0.0))
                }))
            }
            ArithmeticOperator::Multiply { operand_a, operand_b } => {
                (operand_a, operand_b, Arc::new(|a: &Value, b: &Value| {
                    Value::number(a.as_number().unwrap_or(0.0) * b.as_number().unwrap_or(0.0))
                }))
            }
            ArithmeticOperator::Divide { operand_a, operand_b } => {
                (operand_a, operand_b, Arc::new(|a: &Value, b: &Value| {
                    let divisor = b.as_number().unwrap_or(1.0);
                    Value::number(a.as_number().unwrap_or(0.0) / divisor)
                }))
            }
            ArithmeticOperator::Negate { operand } => {
                // Unary negate — compile as Map
                let source_name = format!("{}_neg_src", name);
                let source_var = self.compile_reactive_var(&source_name, operand)?;
                self.reactive_vars.shift_remove(&source_name);
                let var = VarId::new(name);
                self.collections.insert(
                    var.clone(),
                    CollectionSpec::Map {
                        source: source_var,
                        f: Arc::new(|v: &Value| Value::number(-v.as_number().unwrap_or(0.0))),
                    },
                );
                self.reactive_vars.insert(name.to_string(), var.clone());
                return Ok(var);
            }
        };

        // Compile both operands as reactive vars
        let left_name = format!("{}_arith_left", name);
        let left_var = self.compile_reactive_var(&left_name, operand_a)?;
        self.reactive_vars.shift_remove(&left_name);
        let right_name = format!("{}_arith_right", name);
        let right_var = self.compile_reactive_var(&right_name, operand_b)?;
        self.reactive_vars.shift_remove(&right_name);

        let var = VarId::new(name);
        self.collections.insert(
            var.clone(),
            CollectionSpec::Join {
                left: left_var,
                right: right_var,
                combine: op_fn,
            },
        );
        self.reactive_vars.insert(name.to_string(), var.clone());
        Ok(var)
    }

    /// Compile a reactive comparison (e.g., `todos_count == completed_todos_count`).
    fn compile_reactive_comparison(
        &mut self,
        name: &str,
        cmp: &static_expression::Comparator,
    ) -> Result<VarId, String> {
        use static_expression::Comparator;
        let (operand_a, operand_b, cmp_fn): (_, _, Arc<dyn Fn(&Value, &Value) -> Value + 'static>) = match cmp {
            Comparator::Equal { operand_a, operand_b } => {
                (operand_a, operand_b, Arc::new(|a: &Value, b: &Value| {
                    if a == b { Value::tag("True") } else { Value::tag("False") }
                }))
            }
            Comparator::NotEqual { operand_a, operand_b } => {
                (operand_a, operand_b, Arc::new(|a: &Value, b: &Value| {
                    if a != b { Value::tag("True") } else { Value::tag("False") }
                }))
            }
            Comparator::Greater { operand_a, operand_b } => {
                (operand_a, operand_b, Arc::new(|a: &Value, b: &Value| {
                    if a > b { Value::tag("True") } else { Value::tag("False") }
                }))
            }
            Comparator::GreaterOrEqual { operand_a, operand_b } => {
                (operand_a, operand_b, Arc::new(|a: &Value, b: &Value| {
                    if a >= b { Value::tag("True") } else { Value::tag("False") }
                }))
            }
            Comparator::Less { operand_a, operand_b } => {
                (operand_a, operand_b, Arc::new(|a: &Value, b: &Value| {
                    if a < b { Value::tag("True") } else { Value::tag("False") }
                }))
            }
            Comparator::LessOrEqual { operand_a, operand_b } => {
                (operand_a, operand_b, Arc::new(|a: &Value, b: &Value| {
                    if a <= b { Value::tag("True") } else { Value::tag("False") }
                }))
            }
        };

        let left_name = format!("{}_cmp_left", name);
        let left_var = self.compile_reactive_var(&left_name, operand_a)?;
        self.reactive_vars.shift_remove(&left_name);
        let right_name = format!("{}_cmp_right", name);
        let right_var = self.compile_reactive_var(&right_name, operand_b)?;
        self.reactive_vars.shift_remove(&right_name);

        let var = VarId::new(name);
        self.collections.insert(
            var.clone(),
            CollectionSpec::Join {
                left: left_var,
                right: right_var,
                combine: cmp_fn,
            },
        );
        self.reactive_vars.insert(name.to_string(), var.clone());
        Ok(var)
    }

    /// Compile a reactive pipe expression.
    fn compile_reactive_pipe(
        &mut self,
        name: &str,
        from: &Spanned<Expression>,
        to: &Spanned<Expression>,
    ) -> Result<VarId, String> {
        match &to.node {
            // Pattern: `initial |> HOLD state { body }`
            Expression::Hold { state_param, body } => {
                self.compile_hold(name, from, state_param.as_str(), body)
            }

            Expression::FunctionCall { path, arguments } => {
                let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                match path_strs.as_slice() {
                    // Pattern: `LATEST { ... } |> Math/sum()`
                    ["Math", "sum"] => {
                        if let Expression::Latest { inputs } = &from.node {
                            self.compile_latest_sum(name, inputs)
                        } else {
                            // Pipe chain ending in Math/sum: compile from, then wrap in sum
                            let pre_sum_name = format!("{}_pre_sum", name);
                            let source_var = self.compile_reactive_var(&pre_sum_name, from)?;
                            self.reactive_vars.shift_remove(&pre_sum_name);
                            let hold_var = self.fresh_var(&format!("{}_sum_hold", name));
                            let initial_var = self.fresh_var(&format!("{}_sum_init", name));
                            self.collections.insert(
                                initial_var.clone(),
                                CollectionSpec::Literal(Value::number(0.0)),
                            );
                            self.collections.insert(
                                hold_var.clone(),
                                CollectionSpec::HoldState {
                                    initial: initial_var,
                                    events: source_var,
                                    initial_value: Value::number(0.0),
                                    transform: Arc::new(|state: &Value, event: &Value| {
                                        Value::number(
                                            state.as_number().unwrap_or(0.0)
                                                + event.as_number().unwrap_or(0.0),
                                        )
                                    }),
                                },
                            );
                            // Skip the initial value (0) — only emit after first event
                            let sum_var = VarId::new(name);
                            self.collections.insert(
                                sum_var.clone(),
                                CollectionSpec::Skip { source: hold_var, count: 1 },
                            );
                            self.reactive_vars.insert(name.to_string(), sum_var.clone());
                            Ok(sum_var)
                        }
                    }

                    // Pattern: `... |> Timer/interval()`
                    ["Timer", "interval"] => {
                        self.compile_timer(name, from)
                    }

                    // Pattern: `... |> Stream/skip(count: N)`
                    ["Stream", "skip"] => {
                        // Compile the source with a temporary name to avoid VarId collision
                        let pre_skip_name = format!("{}_pre_skip", name);
                        let source_var = self.compile_reactive_var(&pre_skip_name, from)?;
                        // Remove the temp name from reactive_vars
                        self.reactive_vars.shift_remove(&pre_skip_name);
                        // Extract skip count
                        let skip_count = self.extract_skip_count(arguments);
                        let skip_var = VarId::new(name);
                        self.collections.insert(
                            skip_var.clone(),
                            CollectionSpec::Skip {
                                source: source_var,
                                count: skip_count,
                            },
                        );
                        self.reactive_vars.insert(name.to_string(), skip_var.clone());
                        Ok(skip_var)
                    }

                    // Pattern: `... |> Document/new()`
                    ["Document", "new"] => {
                        // This is handled in compile_document_expr, not here
                        Err(format!("Document/new() should be handled at document level for '{}'", name))
                    }

                    // Pattern: `... |> Router/go_to()`
                    ["Router", "go_to"] => {
                        let pre_name = format!("{}_pre_goto", name);
                        let source_var = self.compile_reactive_var(&pre_name, from)?;
                        self.reactive_vars.shift_remove(&pre_name);

                        let goto_var = VarId::new(name);
                        self.collections.insert(
                            goto_var.clone(),
                            CollectionSpec::SideEffect {
                                source: source_var,
                                effect: SideEffectKind::RouterGoTo,
                            },
                        );
                        self.reactive_vars.insert(name.to_string(), goto_var.clone());
                        Ok(goto_var)
                    }

                    // Pattern: `... |> List/clear(on: event_source)`
                    // The `from` is typically `LIST {} |> List/append(item: X)`
                    ["List", "clear"] => {
                        self.compile_list_chain(name, from, arguments)
                    }

                    // Pattern: `LIST {} |> List/append(item: X)` (without clear)
                    ["List", "append"] => {
                        self.compile_list_append_only(name, from, arguments)
                    }

                    // Pattern: `source |> List/count()`
                    ["List", "count"] => {
                        self.compile_list_count(name, from)
                    }

                    // Pattern: `source |> List/retain(item, if: predicate)`
                    ["List", "retain"] => {
                        self.compile_list_retain(name, from, arguments)
                    }

                    // Pattern: `source |> List/map(item, new: transform)`
                    ["List", "map"] => {
                        self.compile_list_map(name, from, arguments)
                    }

                    // Pattern: `source |> List/remove(on: event)`
                    ["List", "remove"] => {
                        self.compile_list_remove(name, from, arguments)
                    }

                    _ => {
                        // Try user-defined function: `source |> my_function()`
                        // Compiled as a Map that evaluates the function body per input.
                        if path.len() == 1 {
                            let fn_name = path[0].as_str().to_string();
                            if self.compiler.functions.iter().any(|(n, _, _)| n == &fn_name) {
                                let source_var = self.resolve_reactive_source(from)?;
                                let compiler = self.compiler.clone();
                                let args_clone: Vec<(String, Option<Spanned<Expression>>)> = arguments
                                    .iter()
                                    .map(|a| (a.node.name.as_str().to_string(), a.node.value.clone()))
                                    .collect();
                                let map_var = VarId::new(name);
                                self.collections.insert(
                                    map_var.clone(),
                                    CollectionSpec::Map {
                                        source: source_var,
                                        f: Arc::new(move |input: &Value| {
                                            // Build scope: first positional param = piped input
                                            let func = compiler.functions.iter()
                                                .find(|(n, _, _)| n == &fn_name);
                                            if let Some((_, params, body)) = func {
                                                let mut scope = IndexMap::new();
                                                // Bind the piped value to the first parameter
                                                if let Some(first_param) = params.first() {
                                                    scope.insert(first_param.clone(), input.clone());
                                                }
                                                // Bind explicit arguments
                                                for (arg_name, arg_val) in &args_clone {
                                                    if let Some(val_expr) = arg_val {
                                                        if let Ok(v) = compiler.eval_static_with_scope(val_expr, &scope) {
                                                            scope.insert(arg_name.clone(), v);
                                                        }
                                                    }
                                                }
                                                // Bind by parameter position
                                                for (i, p) in params.iter().enumerate().skip(1) {
                                                    if i - 1 < args_clone.len() {
                                                        if let Some(val_expr) = &args_clone[i - 1].1 {
                                                            if let Ok(v) = compiler.eval_static_with_scope(val_expr, &scope) {
                                                                scope.insert(p.clone(), v);
                                                            }
                                                        }
                                                    }
                                                }
                                                compiler.eval_static_with_scope(body, &scope)
                                                    .unwrap_or(Value::Unit)
                                            } else {
                                                Value::Unit
                                            }
                                        }),
                                    },
                                );
                                self.reactive_vars.insert(name.to_string(), map_var.clone());
                                return Ok(map_var);
                            }
                        }
                        Err(format!("Unsupported function in reactive pipe for '{}': {}",
                            name, path_strs.join("/")))
                    },
                }
            }

            // Pattern: `reactive_source |> WHEN { arms }`
            Expression::When { arms } => {
                // Check if this WHEN has SKIP arms and text-dependent body
                // (needs FlatMap + HoldState instead of simple Map)
                if self.when_has_skip_and_text_dep(from, arms) {
                    return self.compile_text_dependent_when(name, from, arms);
                }

                let source_var = self.resolve_reactive_source(from)?;
                let when_fn = self.build_when_map_fn(arms);
                let when_var = VarId::new(name);
                self.collections.insert(
                    when_var.clone(),
                    CollectionSpec::Map {
                        source: source_var,
                        f: when_fn,
                    },
                );
                self.reactive_vars.insert(name.to_string(), when_var.clone());
                Ok(when_var)
            }

            // Pattern: `event_source |> THEN { body }`
            Expression::Then { body } => {
                let (source_var, _) = self.compile_event_source(from)?;
                let transform = self.build_then_transform(body);
                let then_var = VarId::new(name);
                self.collections.insert(
                    then_var.clone(),
                    CollectionSpec::Then {
                        source: source_var,
                        body: transform,
                    },
                );
                self.reactive_vars.insert(name.to_string(), then_var.clone());
                Ok(then_var)
            }

            // Pattern: `reactive_source |> WHILE { arms }`
            // In DD, WHILE is semantically a Map — input changes → output changes to matched arm
            Expression::While { arms } => {
                let source_var = self.resolve_reactive_source(from)?;
                let while_fn = self.build_when_map_fn(arms);
                let while_var = VarId::new(name);
                self.collections.insert(
                    while_var.clone(),
                    CollectionSpec::Map {
                        source: source_var,
                        f: while_fn,
                    },
                );
                self.reactive_vars.insert(name.to_string(), while_var.clone());
                Ok(while_var)
            }

            _ => {
                Err(format!("Unsupported reactive pipe target for '{}': {:?}",
                    name, std::mem::discriminant(&to.node)))
            }
        }
    }

    /// Compile `Duration[seconds: S] |> Timer/interval()` into a timer input.
    fn compile_timer(
        &mut self,
        name: &str,
        duration_expr: &Spanned<Expression>,
    ) -> Result<VarId, String> {
        // Extract duration seconds from TaggedObject(Duration)
        let seconds = self.extract_duration_seconds(duration_expr)?;

        // Create timer input
        let input_id = self.add_input_with_timer(
            InputKind::Timer,
            Some(name.to_string()),
            Some(seconds),
        );
        let timer_var = VarId::new(name);
        self.collections.insert(
            timer_var.clone(),
            CollectionSpec::Input(input_id),
        );
        self.reactive_vars.insert(name.to_string(), timer_var.clone());
        Ok(timer_var)
    }

    fn extract_duration_seconds(&self, expr: &Spanned<Expression>) -> Result<f64, String> {
        match &expr.node {
            Expression::TaggedObject { tag, object } => {
                if tag.as_str() == "Duration" {
                    for var in &object.variables {
                        if var.node.name.as_str() == "seconds" {
                            return self.compiler.eval_static(&var.node.value)
                                .and_then(|v| v.as_number().ok_or_else(|| "Duration seconds must be a number".to_string()));
                        }
                    }
                    Err("Duration missing 'seconds' field".to_string())
                } else {
                    Err(format!("Expected Duration, got {}", tag.as_str()))
                }
            }
            _ => Err("Expected Duration[seconds: ...] for Timer/interval".to_string()),
        }
    }

    fn extract_skip_count(&self, arguments: &[Spanned<Argument>]) -> usize {
        for arg in arguments {
            if arg.node.name.as_str() == "count" {
                if let Some(ref val_expr) = arg.node.value {
                    if let Ok(val) = self.compiler.eval_static(val_expr) {
                        return val.as_number().unwrap_or(0.0) as usize;
                    }
                }
            }
        }
        0
    }

    fn compile_hold(
        &mut self,
        name: &str,
        initial_expr: &Spanned<Expression>,
        state_param: &str,
        body: &Spanned<Expression>,
    ) -> Result<VarId, String> {
        // Evaluate initial value statically
        let initial_value = self.compiler.eval_static(initial_expr)
            .map_err(|e| format!("Cannot evaluate initial value for '{}': {}", name, e))?;

        // Check for persisted value to override initial
        let effective_initial = self.persisted_holds
            .get(name)
            .cloned()
            .unwrap_or_else(|| initial_value.clone());

        // Create the initial collection
        let initial_var = self.fresh_var(&format!("{}_initial", name));
        self.collections.insert(
            initial_var.clone(),
            CollectionSpec::Literal(effective_initial.clone()),
        );

        // Find LINK in the body and create input
        let (events_var, link_path) = self.compile_hold_body_events(name, body)?;

        // Build the transform closure
        let transform = self.build_hold_transform(state_param, body);

        // Create the HOLD collection
        let hold_var = VarId::new(name);
        self.collections.insert(
            hold_var.clone(),
            CollectionSpec::HoldState {
                initial: initial_var,
                events: events_var,
                initial_value: effective_initial,
                transform,
            },
        );

        self.reactive_vars.insert(name.to_string(), hold_var.clone());

        // Emit persistence side effect if storage_key is set
        if let Some(key) = self.storage_key.clone() {
            let persist_var = self.fresh_var(&format!("{}_persist", name));
            self.collections.insert(
                persist_var,
                CollectionSpec::SideEffect {
                    source: hold_var.clone(),
                    effect: SideEffectKind::PersistHold {
                        key,
                        hold_name: name.to_string(),
                    },
                },
            );
        }

        Ok(hold_var)
    }

    fn compile_hold_body_events(
        &mut self,
        hold_name: &str,
        body: &Spanned<Expression>,
    ) -> Result<(VarId, Option<String>), String> {
        // Body is typically: `event_source |> THEN { transform }`
        match &body.node {
            Expression::Pipe { from, to } => {
                match &to.node {
                    Expression::Then { body: then_body } => {
                        // `from` is the event source (e.g., increment_button.event.press)
                        let (source_var, link_path) = self.compile_event_source(from)?;

                        // Create a THEN collection
                        let then_var = self.fresh_var(&format!("{}_then", hold_name));
                        let transform = self.build_then_transform(then_body);
                        self.collections.insert(
                            then_var.clone(),
                            CollectionSpec::Then {
                                source: source_var,
                                body: transform,
                            },
                        );

                        Ok((then_var, link_path))
                    }
                    _ => {
                        // Direct event source without THEN
                        self.compile_event_source(from)
                    }
                }
            }
            // LATEST { stream1 |> THEN { val1 }, stream2 |> THEN { val2 }, ... }
            // Merge multiple event sources into one.
            Expression::Latest { inputs } => {
                let mut event_vars = Vec::new();
                for (i, input) in inputs.iter().enumerate() {
                    let sub_name = format!("{}__latest_{}", hold_name, i);
                    let (var_id, _) = self.compile_hold_body_events(&sub_name, input)?;
                    event_vars.push(var_id);
                }
                if event_vars.is_empty() {
                    return Err(format!("Empty LATEST in HOLD body for '{}'", hold_name));
                }
                if event_vars.len() == 1 {
                    return Ok((event_vars.remove(0), None));
                }
                // Concat all event streams
                let concat_var = self.fresh_var(&format!("{}_events_concat", hold_name));
                self.collections.insert(
                    concat_var.clone(),
                    CollectionSpec::Concat(event_vars),
                );
                // HoldLatest to keep only the most recent event
                let latest_var = self.fresh_var(&format!("{}_events_latest", hold_name));
                self.collections.insert(
                    latest_var.clone(),
                    CollectionSpec::HoldLatest(vec![concat_var]),
                );
                Ok((latest_var, None))
            }

            _ => {
                Err(format!("Unsupported HOLD body pattern for '{}'", hold_name))
            }
        }
    }

    /// Detect event type and link_path from an alias path.
    ///
    /// Parses paths like:
    /// - `store.elements.item_input.event.key_down.key` → (KeyDown, `store.elements.item_input.event.key_down`)
    /// - `store.elements.clear_button.event.press` → (LinkPress, `store.elements.clear_button.event.press`)
    /// - `store.elements.item_input.event.change.text` → (TextChange, `store.elements.item_input.event.change`)
    fn detect_event_kind_and_path(full_path: &str) -> (InputKind, String) {
        // Event type suffixes that the bridge uses for link_path
        let event_types = [
            (".event.key_down", InputKind::KeyDown),
            (".event.change", InputKind::TextChange),
            (".event.press", InputKind::LinkPress),
            (".event.click", InputKind::LinkClick),
            (".event.blur", InputKind::Blur),
            (".event.focus", InputKind::Focus),
            (".event.double_click", InputKind::DoubleClick),
        ];
        for (suffix, kind) in &event_types {
            if let Some(pos) = full_path.find(suffix) {
                // link_path = everything up to and including the event type (no field suffix)
                let link_path = &full_path[..pos + suffix.len()];
                return (kind.clone(), link_path.to_string());
            }
        }
        // Default: use full path as-is, assume LinkPress
        (InputKind::LinkPress, full_path.to_string())
    }

    fn compile_event_source(
        &mut self,
        expr: &Spanned<Expression>,
    ) -> Result<(VarId, Option<String>), String> {
        match &expr.node {
            Expression::Alias(Alias::WithoutPassed { parts, .. }) => {
                let path_str: String = parts
                    .iter()
                    .map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join(".");

                let var_name = parts[0].as_str();

                // Check if this references an already-compiled reactive variable (e.g., timer)
                if parts.len() == 1 {
                    if let Some(var_id) = self.reactive_vars.get(var_name) {
                        return Ok((var_id.clone(), None));
                    }
                }

                // Build the effective path (with scope prefix if needed)
                // Avoid double-prepend if the path already starts with the prefix
                let effective_path = if let Some(ref prefix) = self.scope_prefix {
                    if path_str.starts_with(&format!("{}.", prefix)) {
                        path_str.clone()
                    } else {
                        format!("{}.{}", prefix, path_str)
                    }
                } else {
                    path_str.clone()
                };

                // Check if the path contains an event reference
                if effective_path.contains(".event.") {
                    let (kind, link_path) = Self::detect_event_kind_and_path(&effective_path);
                    let input_id = self.add_input(kind, Some(link_path.clone()));
                    let input_var = self.fresh_var("link_input");
                    self.collections.insert(
                        input_var.clone(),
                        CollectionSpec::Input(input_id),
                    );
                    return Ok((input_var, Some(link_path)));
                }

                // Check if this references a variable with a LINK element
                if let Some(var_expr) = self.compiler.get_var_expr(var_name) {
                    if self.compiler.expr_contains_link(var_expr) {
                        let input_id = self.add_input(
                            InputKind::LinkPress,
                            Some(effective_path.clone()),
                        );
                        let input_var = self.fresh_var("link_input");
                        self.collections.insert(
                            input_var.clone(),
                            CollectionSpec::Input(input_id),
                        );
                        return Ok((input_var, Some(effective_path)));
                    }
                }

                // Try with scope prefix for nested object references
                if let Some(ref prefix) = self.scope_prefix {
                    let prefixed_var_name = format!("{}.{}", prefix, var_name);
                    if let Some(var_expr) = self.compiler.get_var_expr(&prefixed_var_name) {
                        if self.compiler.expr_contains_link(var_expr) {
                            let input_id = self.add_input(
                                InputKind::LinkPress,
                                Some(effective_path.clone()),
                            );
                            let input_var = self.fresh_var("link_input");
                            self.collections.insert(
                                input_var.clone(),
                                CollectionSpec::Input(input_id),
                            );
                            return Ok((input_var, Some(effective_path)));
                        }
                    }
                }

                Err(format!("Event source '{}' not found as reactive var or LINK element", path_str))
            }
            // Inline reactive pipe as event source (e.g., Duration |> Timer/interval())
            Expression::Pipe { .. } => {
                let inline_name = format!("__inline_event_{}", self.next_anon_id);
                let var_id = self.compile_reactive_var(&inline_name, expr)?;
                Ok((var_id, None))
            }
            _ => {
                Err(format!("Unsupported event source expression: {:?}",
                    std::mem::discriminant(&expr.node)))
            }
        }
    }

    fn compile_latest(
        &mut self,
        name: &str,
        inputs: &[Spanned<Expression>],
    ) -> Result<VarId, String> {
        let mut source_vars = Vec::new();

        for (i, input) in inputs.iter().enumerate() {
            let var = self.compile_latest_input(name, i, input)?;
            source_vars.push(var);
        }

        let latest_var = VarId::new(name);
        self.collections.insert(
            latest_var.clone(),
            CollectionSpec::HoldLatest(source_vars),
        );

        self.reactive_vars.insert(name.to_string(), latest_var.clone());
        Ok(latest_var)
    }

    fn compile_latest_input(
        &mut self,
        parent_name: &str,
        index: usize,
        expr: &Spanned<Expression>,
    ) -> Result<VarId, String> {
        match &expr.node {
            Expression::Literal(lit) => {
                let val = Compiler::eval_literal(lit);
                let var = self.fresh_var(&format!("{}_lit{}", parent_name, index));
                self.collections.insert(var.clone(), CollectionSpec::Literal(val));
                Ok(var)
            }
            Expression::Pipe { from, to } => {
                match &to.node {
                    Expression::Then { body } => {
                        let (source_var, _) = self.compile_event_source(from)?;
                        let transform = self.build_then_transform(body);
                        let then_var = self.fresh_var(&format!("{}_then{}", parent_name, index));
                        self.collections.insert(
                            then_var.clone(),
                            CollectionSpec::Then { source: source_var, body: transform },
                        );
                        Ok(then_var)
                    }
                    _ => Err(format!("Unsupported LATEST input pipe")),
                }
            }
            _ => Err(format!("Unsupported LATEST input: {:?}", std::mem::discriminant(&expr.node))),
        }
    }

    /// Compile `LATEST { initial, events... } |> Math/sum()`.
    ///
    /// This is a sum accumulator: the first literal in LATEST is the initial value,
    /// events from button presses (via THEN) are added to the running sum.
    fn compile_latest_sum(
        &mut self,
        name: &str,
        inputs: &[Spanned<Expression>],
    ) -> Result<VarId, String> {
        // Find the initial literal value (first Literal in LATEST inputs)
        let mut initial_value = Value::number(0.0);
        let mut event_vars = Vec::new();

        for (i, input) in inputs.iter().enumerate() {
            match &input.node {
                Expression::Literal(Literal::Number(n)) => {
                    initial_value = Value::number(*n);
                    // Also create a literal collection (it's the first value in the sum)
                    // The initial literal IS the starting value, not an event
                }
                _ => {
                    // Compile as event source
                    let var = self.compile_latest_input(name, i, input)?;
                    event_vars.push(var);
                }
            }
        }

        // Check for persisted value to override initial
        let effective_initial = self.persisted_holds
            .get(name)
            .cloned()
            .unwrap_or_else(|| initial_value.clone());

        // Create initial value collection
        let initial_var = self.fresh_var(&format!("{}_sum_init", name));
        self.collections.insert(
            initial_var.clone(),
            CollectionSpec::Literal(effective_initial.clone()),
        );

        // Concat all event sources
        let events_var = if event_vars.len() == 1 {
            event_vars[0].clone()
        } else {
            let concat_var = self.fresh_var(&format!("{}_sum_events", name));
            self.collections.insert(
                concat_var.clone(),
                CollectionSpec::Concat(event_vars),
            );
            concat_var
        };

        // Create HoldState with sum transform
        let sum_var = VarId::new(name);
        self.collections.insert(
            sum_var.clone(),
            CollectionSpec::HoldState {
                initial: initial_var,
                events: events_var,
                initial_value: effective_initial,
                transform: Arc::new(|state: &Value, event: &Value| {
                    let s = state.as_number().unwrap_or(0.0);
                    let e = event.as_number().unwrap_or(0.0);
                    Value::number(s + e)
                }),
            },
        );

        self.reactive_vars.insert(name.to_string(), sum_var.clone());

        // Emit persistence side effect if storage_key is set
        if let Some(key) = self.storage_key.clone() {
            let persist_var = self.fresh_var(&format!("{}_persist", name));
            self.collections.insert(
                persist_var,
                CollectionSpec::SideEffect {
                    source: sum_var.clone(),
                    effect: SideEffectKind::PersistHold {
                        key,
                        hold_name: name.to_string(),
                    },
                },
            );
        }

        Ok(sum_var)
    }

    fn compile_document_expr(
        &mut self,
        expr: &Spanned<Expression>,
    ) -> Result<VarId, String> {
        // Handle `reactive_var |> Document/new()` pipe pattern
        if let Expression::Pipe { from, to } = &expr.node {
            if let Expression::FunctionCall { path, arguments } = &to.node {
                let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                if path_strs.as_slice() == ["Document", "new"] && arguments.is_empty() {
                    // Try simple alias first (e.g., `counter |> Document/new()`)
                    if let Ok(var) = self.compile_piped_document(from) {
                        return Ok(var);
                    }
                    // Fall through to chain compilation for pipe expressions
                    return self.compile_piped_document_chain(expr);
                }
            }
            // Check for longer pipe chains ending in Document/new()
            // e.g., `Timer |> THEN |> Math/sum |> Document/new()`
            if self.is_reactive_pipe(expr) {
                return self.compile_piped_document_chain(expr);
            }
        }

        // Standard pattern: `Document/new(root: some_element)`
        // Filter to only reactive vars with guaranteed initial values.
        // Event-only vars (LINK inputs, WHEN with SKIP) would block the Join chain.
        let reactive_deps: Vec<String> = self.reactive_vars.iter()
            .filter(|(_, var_id)| self.has_initial_value(var_id))
            .map(|(name, _)| name.clone())
            .collect();


        if reactive_deps.is_empty() {
            // Static document
            let val = self.compiler.eval_static(expr)
                .map_err(|e| format!("Cannot evaluate document: {}", e))?;
            let doc_var = self.fresh_var("document");
            self.collections.insert(doc_var.clone(), CollectionSpec::Literal(val));
            return Ok(doc_var);
        }

        // For single reactive dependency (common case: counter_hold, counter)
        if reactive_deps.len() == 1 {
            let dep_name = &reactive_deps[0];
            let dep_var = self.reactive_vars.get(dep_name).unwrap().clone();

            // Build a document template closure
            let doc_closure = self.build_document_closure(dep_name, expr)?;

            let doc_var = self.fresh_var("document");
            self.collections.insert(
                doc_var.clone(),
                CollectionSpec::Map {
                    source: dep_var,
                    f: doc_closure,
                },
            );
            return Ok(doc_var);
        }

        // Multiple reactive dependencies — try find root and derive others first
        match self.find_root_and_derived(&reactive_deps) {
            Ok((root_dep, derived_deps)) => {
                let root_var = self.reactive_vars.get(&root_dep).unwrap().clone();

                // Build derived variable computations (closures that derive from root value)
                let mut derived_fns: Vec<(String, Arc<dyn Fn(&Value) -> Value + 'static>)> = Vec::new();
                let mut all_derived_handled = true;
                for dep_name in &derived_deps {
                    let mut handled = false;
                    if let Some(dep_expr) = self.compiler.get_var_expr(dep_name).cloned() {
                        if let Expression::Pipe { to, .. } = &dep_expr.node {
                            if let Expression::When { arms } = &to.node {
                                let when_fn = self.build_when_map_fn(arms);
                                derived_fns.push((dep_name.clone(), when_fn));
                                handled = true;
                            }
                        }
                    }
                    if !handled {
                        all_derived_handled = false;
                        break;
                    }
                }

                if !all_derived_handled {
                    // Not all derived deps are simple WHEN transforms of root.
                    // Fall through to multi-dep Join approach.
                    return self.compile_multi_dep_document(expr, &reactive_deps);
                }

                // Build document closure that evaluates with reactive values in scope
                #[cfg(target_arch = "wasm32")]
                zoon::println!("[DD v2] compile_document_expr: root-derived path, root={}", root_dep);
                let compiler_clone = self.compiler.clone();
                let doc_expr_clone = expr.clone();
                let root_dep_name = root_dep.clone();

                let doc_closure: Arc<dyn Fn(&Value) -> Value + 'static> =
                    Arc::new(move |root_value: &Value| {
                        let mut scope = IndexMap::new();
                        scope.insert(root_dep_name.clone(), root_value.clone());
                        for (name, f) in &derived_fns {
                            let val = f(root_value);
                            scope.insert(name.clone(), val);
                        }
                        compiler_clone
                            .eval_static_with_scope(&doc_expr_clone, &scope)
                            .unwrap_or(Value::Unit)
                    });

                let doc_var = self.fresh_var("document");
                self.collections.insert(
                    doc_var.clone(),
                    CollectionSpec::Map {
                        source: root_var,
                        f: doc_closure,
                    },
                );
                return Ok(doc_var);
            }
            Err(_) => {
                // Independent deps — use Join to combine them, then Map to document
                return self.compile_multi_dep_document(expr, &reactive_deps);
            }
        }
    }

    /// Compile `reactive_var |> Document/new()` where the from is a simple alias.
    fn compile_piped_document(
        &mut self,
        from: &Spanned<Expression>,
    ) -> Result<VarId, String> {
        // Check if `from` references a reactive variable
        if let Expression::Alias(Alias::WithoutPassed { parts, .. }) = &from.node {
            if parts.len() == 1 {
                let name = parts[0].as_str();
                if let Some(source_var) = self.reactive_vars.get(name).cloned() {
                    // Map the reactive value into a DocumentNew wrapper
                    let doc_var = self.fresh_var("document");
                    self.collections.insert(
                        doc_var.clone(),
                        CollectionSpec::Map {
                            source: source_var,
                            f: Arc::new(|v: &Value| {
                                Value::tagged("DocumentNew", [("root", v.clone())])
                            }),
                        },
                    );
                    return Ok(doc_var);
                }
            }
        }
        Err("Unsupported piped document source".to_string())
    }

    /// Compile a full reactive pipe chain as the document.
    /// e.g., `Duration[seconds: 1] |> Timer/interval() |> THEN { 1 } |> Math/sum() |> Document/new()`
    fn compile_piped_document_chain(
        &mut self,
        expr: &Spanned<Expression>,
    ) -> Result<VarId, String> {
        // Peel off the outer `|> Document/new()` and compile the inner chain as a reactive var
        if let Expression::Pipe { from, to } = &expr.node {
            if let Expression::FunctionCall { path, .. } = &to.node {
                let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                if path_strs.as_slice() == ["Document", "new"] {
                    // Compile the inner chain as a reactive variable named "__doc_source"
                    let source_var = self.compile_reactive_var("__doc_source", from)?;
                    // Wrap in DocumentNew
                    let doc_var = self.fresh_var("document");
                    self.collections.insert(
                        doc_var.clone(),
                        CollectionSpec::Map {
                            source: source_var,
                            f: Arc::new(|v: &Value| {
                                Value::tagged("DocumentNew", [("root", v.clone())])
                            }),
                        },
                    );
                    return Ok(doc_var);
                }
            }
            // Recurse: the inner pipe might still have Document/new deeper
            // This handles chains like `a |> b |> c |> Document/new()`
        }
        Err("Cannot compile piped document chain".to_string())
    }

    fn is_reactive_pipe(&self, expr: &Spanned<Expression>) -> bool {
        match &expr.node {
            Expression::Pipe { from, to } => {
                self.is_reactive_pipe(from) || self.is_reactive_pipe(to)
            }
            Expression::FunctionCall { path, .. } => {
                let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                matches!(path_strs.as_slice(),
                    ["Timer", "interval"] | ["Math", "sum"] | ["Stream", "skip"] | ["Document", "new"])
            }
            Expression::Then { .. } | Expression::Hold { .. } | Expression::Latest { .. } => true,
            Expression::TaggedObject { tag, .. } => tag.as_str() == "Duration",
            _ => false,
        }
    }

    fn build_document_closure(
        &self,
        reactive_var_name: &str,
        doc_expr: &Spanned<Expression>,
    ) -> Result<Arc<dyn Fn(&Value) -> Value + 'static>, String> {
        let doc_template = self.compiler.build_doc_template_keyed(
            reactive_var_name,
            doc_expr,
            self.keyed_display_list_name.as_deref(),
        )?;
        Ok(Arc::new(move |reactive_value: &Value| {
            doc_template.instantiate(reactive_value)
        }))
    }

    /// Compile a document with multiple independent reactive dependencies via Join.
    ///
    /// Joins all reactive deps into a combined value, then Maps to the document.
    /// The combined value is an Object with each dep's name → value.
    fn compile_multi_dep_document(
        &mut self,
        doc_expr: &Spanned<Expression>,
        reactive_deps: &[String],
    ) -> Result<VarId, String> {
        // Filter to real deps (skip side effects, LINKs, internal state vars)
        let real_dep_names: Vec<String> = reactive_deps
            .iter()
            .filter(|name| {
                // Skip __state internal vars
                if name.contains(".__state") {
                    return false;
                }
                // Skip side-effect collections
                if let Some(var_id) = self.reactive_vars.get(*name) {
                    if let Some(spec) = self.collections.get(var_id) {
                        if matches!(spec, CollectionSpec::SideEffect { .. }) {
                            return false;
                        }
                    }
                }
                // Skip LINK markers
                if let Some(expr) = self.compiler.get_var_expr(name) {
                    if matches!(expr.node, Expression::Link) {
                        return false;
                    }
                }
                true
            })
            .cloned()
            .collect();

        if real_dep_names.is_empty() {
            return Err("No real reactive deps for document".to_string());
        }

        if real_dep_names.len() == 1 {
            // Only one real dep after filtering — use simple Map
            let dep_name = &real_dep_names[0];
            let dep_var = self.reactive_vars.get(dep_name).unwrap().clone();
            let doc_closure = self.build_multi_dep_doc_closure(doc_expr, &real_dep_names)?;
            let doc_var = self.fresh_var("document");
            self.collections.insert(
                doc_var.clone(),
                CollectionSpec::Map {
                    source: dep_var,
                    f: Arc::new(move |v: &Value| {
                        let mut combined = BTreeMap::new();
                        combined.insert(Arc::from("__dep_0"), v.clone());
                        doc_closure(&Value::Object(Arc::new(combined)))
                    }),
                },
            );
            return Ok(doc_var);
        }

        // Build a chain of Joins to combine all deps
        // Join is binary, so for N deps we build a chain: Join(Join(d0, d1), d2), ...
        let first_var = self.reactive_vars.get(&real_dep_names[0]).unwrap().clone();
        let second_var = self.reactive_vars.get(&real_dep_names[1]).unwrap().clone();
        let mut current_var = self.fresh_var("joined_deps");
        self.collections.insert(
            current_var.clone(),
            CollectionSpec::Join {
                left: first_var,
                right: second_var,
                combine: Arc::new(move |a: &Value, b: &Value| {
                    Value::object([("__dep_0", a.clone()), ("__dep_1", b.clone())])
                }),
            },
        );

        for (i, dep_name) in real_dep_names.iter().enumerate().skip(2) {
            let dep_var = self.reactive_vars.get(dep_name).unwrap().clone();
            let prev = current_var.clone();
            current_var = self.fresh_var("joined_deps");
            let idx = i;
            self.collections.insert(
                current_var.clone(),
                CollectionSpec::Join {
                    left: prev,
                    right: dep_var,
                    combine: Arc::new(move |combined: &Value, new_dep: &Value| {
                        let key = format!("__dep_{}", idx);
                        combined.update_field(&key, new_dep.clone())
                    }),
                },
            );
        }

        // Map from joined deps to document
        let doc_closure = self.build_multi_dep_doc_closure(doc_expr, &real_dep_names)?;
        let doc_var = self.fresh_var("document");
        self.collections.insert(
            doc_var.clone(),
            CollectionSpec::Map {
                source: current_var,
                f: doc_closure,
            },
        );
        Ok(doc_var)
    }

    /// Build a document closure that receives a combined deps value and produces the document.
    fn build_multi_dep_doc_closure(
        &self,
        doc_expr: &Spanned<Expression>,
        dep_names: &[String],
    ) -> Result<Arc<dyn Fn(&Value) -> Value + 'static>, String> {
        let compiler = self.compiler.clone();
        let doc_expr = doc_expr.clone();
        let dep_names: Vec<String> = dep_names.to_vec();

        // Also collect internal state vars that provide display text etc.
        let mut state_var_names: Vec<(String, String)> = Vec::new();
        for name in &dep_names {
            let state_key = format!("{}.__state", name);
            if self.reactive_vars.contains_key(&state_key) {
                state_var_names.push((name.clone(), state_key));
            }
        }

        // Build static default values for ALL store fields (LINK stubs, static values).
        // The closure will override reactive dep fields with live values.
        let mut link_defaults: Vec<(String, Value)> = Vec::new();
        for (var_name, var_expr) in &self.compiler.variables {
            if let Some(field_name) = var_name.strip_prefix("store.") {
                // Skip reactive deps — they'll be provided from the combined value
                if dep_names.iter().any(|d| d == var_name) {
                    continue;
                }
                // Try static evaluation for the default value
                match self.compiler.eval_static_with_scope(var_expr, &IndexMap::new()) {
                    Ok(val) => {
                        link_defaults.push((field_name.to_string(), val));
                    }
                    Err(_) => {
                        // Can't evaluate statically — skip (LINK evaluates to Value::tag("LINK"))
                    }
                }
            }
        }

        // Collect LINK variable paths for injection into the store object.
        // These create __link_path__ fields so the bridge can route events back.
        let mut link_injections: Vec<(String, String)> = Vec::new(); // (nested_path, full_path)
        for (var_name, var_expr) in &self.compiler.variables {
            if matches!(var_expr.node, Expression::Link) {
                if let Some(rest) = var_name.strip_prefix("store.") {
                    link_injections.push((rest.to_string(), var_name.clone()));
                }
            }
        }

        Ok(Arc::new(move |combined: &Value| {
            let mut scope = IndexMap::new();

            // Reconstruct the scope from combined deps
            // For store-style programs, the deps are like "store.text_to_add" and "store.items"
            // We need to build the "store" object with these fields
            let mut store_fields: BTreeMap<Arc<str>, Value> = BTreeMap::new();

            for (i, name) in dep_names.iter().enumerate() {
                let dep_key = format!("__dep_{}", i);
                let val = combined.get_field(&dep_key).cloned().unwrap_or(Value::Unit);

                // Parse dotted names: "store.items" → insert into store.items
                let parts: Vec<&str> = name.split('.').collect();
                if parts.len() == 2 && parts[0] == "store" {
                    store_fields.insert(Arc::from(parts[1]), val.clone());
                }
                scope.insert(name.clone(), val);
            }

            // If we built store fields, also insert the full store object
            if !store_fields.is_empty() {
                // Add LINK stubs and static field defaults
                for (field_name, default_value) in &link_defaults {
                    if !store_fields.contains_key(field_name.as_str()) {
                        store_fields.insert(Arc::from(field_name.as_str()), default_value.clone());
                    }
                }

                let mut store_value = Value::Object(Arc::new(store_fields));

                // Inject __link_path__ for LINK variables (top-level buttons, filter links, etc.)
                for (nested_path, full_path) in &link_injections {
                    store_value = set_nested_field(&store_value, nested_path, Value::object([
                        ("__link_path__", Value::text(full_path.as_str())),
                    ]));
                }

                // Inject per-item link paths for list fields (todo items need keyed paths
                // like "store.todos.0001.todo_elements.todo_checkbox" for wildcard routing)
                if let Value::Object(ref fields) = store_value {
                    let fields_snapshot = fields.clone();
                    for (field_name, field_val) in fields_snapshot.iter() {
                        if let Value::Tagged { tag, fields: list_fields } = field_val {
                            if tag.as_ref() == "List" {
                                let list_path = format!("store.{}", field_name);
                                let new_list_fields: BTreeMap<Arc<str>, Value> = list_fields.iter()
                                    .map(|(key, item)| {
                                        let new_item = inject_item_link_paths_with_key(
                                            item, &list_path, key.as_ref(),
                                        );
                                        (key.clone(), new_item)
                                    })
                                    .collect();
                                store_value = store_value.update_field(
                                    field_name.as_ref(),
                                    Value::Tagged {
                                        tag: tag.clone(),
                                        fields: Arc::new(new_list_fields),
                                    },
                                );
                            }
                        }
                    }
                }

                scope.insert("store".to_string(), store_value);
            }

            compiler
                .eval_static_with_scope(&doc_expr, &scope)
                .unwrap_or(Value::Unit)
        }))
    }

    fn build_hold_transform(
        &self,
        state_name: &str,
        body: &Spanned<Expression>,
    ) -> Arc<dyn Fn(&Value, &Value) -> Value + 'static> {
        // Extract the transform pattern from the HOLD body
        let transform = self.extract_hold_body_transform(state_name, body);
        match transform {
            HoldTransform::Increment(n) => Arc::new(move |state: &Value, _event: &Value| {
                let current = state.as_number().unwrap_or(0.0);
                Value::number(current + n)
            }),
            HoldTransform::Custom => {
                // General transform: evaluate the THEN body with state in scope.
                // Extract the THEN body expression from the HOLD body.
                let then_body = match &body.node {
                    Expression::Pipe { to, .. } => match &to.node {
                        Expression::Then { body: then_body } => Some(then_body.as_ref().clone()),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(then_expr) = then_body {
                    let compiler = self.compiler.clone();
                    let sname = state_name.to_string();
                    Arc::new(move |state: &Value, _event: &Value| {
                        let mut scope = IndexMap::new();
                        scope.insert(sname.clone(), state.clone());
                        compiler.eval_static_with_scope(&then_expr, &scope)
                            .unwrap_or_else(|_| {
                                // Last resort fallback: increment by 1
                                let current = state.as_number().unwrap_or(0.0);
                                Value::number(current + 1.0)
                            })
                    })
                } else if matches!(&body.node, Expression::Latest { .. }) {
                    // LATEST body: each LATEST input produces the new state via THEN.
                    // The event value IS the new state — just replace.
                    Arc::new(|_state: &Value, event: &Value| {
                        event.clone()
                    })
                } else {
                    // No THEN body found — event replaces state
                    Arc::new(|_state: &Value, event: &Value| {
                        event.clone()
                    })
                }
            }
        }
    }

    fn extract_hold_body_transform(
        &self,
        state_name: &str,
        body: &Spanned<Expression>,
    ) -> HoldTransform {
        match &body.node {
            Expression::Pipe { to, .. } => match &to.node {
                Expression::Then { body: then_body } => {
                    self.extract_arithmetic_transform(state_name, then_body)
                }
                _ => HoldTransform::Custom,
            },
            _ => HoldTransform::Custom,
        }
    }

    fn extract_arithmetic_transform(
        &self,
        state_name: &str,
        expr: &Spanned<Expression>,
    ) -> HoldTransform {
        match &expr.node {
            Expression::ArithmeticOperator(ArithmeticOperator::Add {
                operand_a,
                operand_b,
            }) => {
                if is_alias_named(&operand_a.node, state_name) {
                    if let Expression::Literal(Literal::Number(n)) = &operand_b.node {
                        return HoldTransform::Increment(*n);
                    }
                }
                if is_alias_named(&operand_b.node, state_name) {
                    if let Expression::Literal(Literal::Number(n)) = &operand_a.node {
                        return HoldTransform::Increment(*n);
                    }
                }
                HoldTransform::Custom
            }
            _ => HoldTransform::Custom,
        }
    }

    fn build_then_transform(
        &self,
        body: &Spanned<Expression>,
    ) -> Arc<dyn Fn(&Value) -> Value + 'static> {
        // Try to statically evaluate the THEN body.
        // If it's a constant (like `1`), return that constant.
        // If it depends on state (like `counter + 1`), the HOLD transform handles it.
        match self.compiler.eval_static(body) {
            Ok(val) => {
                // THEN body is a constant — return it on each event
                Arc::new(move |_event: &Value| val.clone())
            }
            Err(_) => {
                // Cannot statically evaluate — return event marker
                // (HOLD body transform handles the actual computation)
                Arc::new(|_event: &Value| Value::tag("Event"))
            }
        }
    }

    /// Resolve an alias expression to its reactive VarId.
    fn resolve_reactive_source(
        &mut self,
        expr: &Spanned<Expression>,
    ) -> Result<VarId, String> {
        if let Expression::Alias(Alias::WithoutPassed { parts, .. }) = &expr.node {
            if parts.len() == 1 {
                let name = parts[0].as_str();
                if let Some(var_id) = self.reactive_vars.get(name) {
                    return Ok(var_id.clone());
                }
                // Try with scope prefix (e.g., "title_to_add" → "store.title_to_add")
                if let Some(ref prefix) = self.scope_prefix {
                    let prefixed = format!("{}.{}", prefix, name);
                    if let Some(var_id) = self.reactive_vars.get(&prefixed) {
                        return Ok(var_id.clone());
                    }
                }
            }
            // Multi-part alias — try as event source (LINK event path)
            let (var_id, _) = self.compile_event_source(expr)?;
            return Ok(var_id);
        }
        // Handle FunctionCall as reactive source (e.g., Router/route())
        if let Expression::FunctionCall { path, arguments } = &expr.node {
            let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
            match path_strs.as_slice() {
                ["Router", "route"] => {
                    // Router/route() is an input that receives route change events
                    let input_id = self.add_input(InputKind::Router, Some("__router".to_string()));
                    let router_var = self.fresh_var("__router");
                    self.collections.insert(
                        router_var.clone(),
                        CollectionSpec::Input(input_id),
                    );
                    self.reactive_vars.insert("__router".to_string(), router_var.clone());
                    return Ok(router_var);
                }
                _ => {}
            }
        }
        // Handle Pipe as reactive source (compile the pipe chain)
        if let Expression::Pipe { from, to } = &expr.node {
            let temp_name = self.fresh_var("__pipe_source");
            let var_id = self.compile_reactive_pipe(temp_name.as_str(), from, to)?;
            return Ok(var_id);
        }
        Err(format!(
            "Cannot resolve reactive source: {:?} (expr: {:?})",
            std::mem::discriminant(&expr.node),
            &expr.node,
        ))
    }

    /// Build a WHEN pattern-matching closure for Map collections.
    fn build_when_map_fn(
        &self,
        arms: &[static_expression::Arm],
    ) -> Arc<dyn Fn(&Value) -> Value + 'static> {
        // Pre-compile arms: evaluate each pattern and body to Values at compile time
        let mut compiled_arms: Vec<(Option<Value>, Value)> = Vec::new();

        for arm in arms {
            let pattern_val = self.try_eval_pattern_to_value(&arm.pattern);
            let body_val = self.compiler.eval_static(&arm.body).unwrap_or(Value::Unit);
            compiled_arms.push((pattern_val, body_val));
        }

        Arc::new(move |input: &Value| {
            for (pattern_val, body_val) in &compiled_arms {
                match pattern_val {
                    Some(pv) => {
                        if input == pv {
                            return body_val.clone();
                        }
                    }
                    None => {
                        // Wildcard or binding — always matches
                        return body_val.clone();
                    }
                }
            }
            Value::Unit
        })
    }

    /// Try to convert a WHEN pattern to a concrete Value for equality matching.
    /// Returns None for wildcards/bindings (which match anything).
    fn try_eval_pattern_to_value(
        &self,
        pattern: &static_expression::Pattern,
    ) -> Option<Value> {
        match pattern {
            static_expression::Pattern::Literal(lit) => Some(Compiler::eval_literal(lit)),
            static_expression::Pattern::Alias { name } => {
                let s = name.as_str();
                if s == "__" || s == "_" {
                    None // wildcard
                } else if s.chars().next().map_or(false, |c| c.is_uppercase()) {
                    // Uppercase = tag match (e.g., Home, About)
                    Some(Value::tag(s))
                } else {
                    None // lowercase = variable binding (matches anything)
                }
            }
            _ => None,
        }
    }

    /// Find the root reactive dependency among multiple deps.
    /// Returns (root_name, derived_dep_names).
    fn find_root_and_derived(
        &self,
        deps: &[String],
    ) -> Result<(String, Vec<String>), String> {
        // Filter out side-effect collections and LINK markers
        let real_deps: Vec<&String> = deps
            .iter()
            .filter(|name| {
                // Skip side-effect collections
                if let Some(var_id) = self.reactive_vars.get(*name) {
                    if let Some(spec) = self.collections.get(var_id) {
                        if matches!(spec, CollectionSpec::SideEffect { .. }) {
                            return false;
                        }
                    }
                }
                // Skip LINK markers
                if let Some(expr) = self.compiler.get_var_expr(name) {
                    if matches!(expr.node, Expression::Link) {
                        return false;
                    }
                }
                true
            })
            .collect();

        if real_deps.len() == 1 {
            return Ok((real_deps[0].clone(), Vec::new()));
        }

        // Find the root: a dep whose expression doesn't reference any other dep
        for dep in &real_deps {
            let is_root = real_deps.iter().all(|other| {
                if dep == other {
                    return true;
                }
                if let Some(expr) = self.compiler.get_var_expr(dep) {
                    !Self::expr_references_var(expr, other)
                } else {
                    true
                }
            });
            if is_root {
                let derived = real_deps
                    .iter()
                    .filter(|d| d != &dep)
                    .map(|d| (*d).clone())
                    .collect();
                return Ok(((*dep).clone(), derived));
            }
        }

        Err(format!(
            "Cannot find root reactive dependency among: {:?}",
            real_deps
        ))
    }

    /// Check if an expression references a variable by name.
    fn expr_references_var(expr: &Spanned<Expression>, var_name: &str) -> bool {
        match &expr.node {
            Expression::Alias(Alias::WithoutPassed { parts, .. }) => {
                parts.len() >= 1 && parts[0].as_str() == var_name
            }
            Expression::Pipe { from, to } => {
                Self::expr_references_var(from, var_name)
                    || Self::expr_references_var(to, var_name)
            }
            Expression::FunctionCall { arguments, .. } => arguments.iter().any(|a| {
                a.node
                    .value
                    .as_ref()
                    .map(|v| Self::expr_references_var(v, var_name))
                    .unwrap_or(false)
            }),
            Expression::List { items } => {
                items.iter().any(|i| Self::expr_references_var(i, var_name))
            }
            Expression::Object(obj) => obj
                .variables
                .iter()
                .any(|v| Self::expr_references_var(&v.node.value, var_name)),
            Expression::Block { variables, output } => {
                variables
                    .iter()
                    .any(|v| Self::expr_references_var(&v.node.value, var_name))
                    || Self::expr_references_var(output, var_name)
            }
            _ => false,
        }
    }

    // -----------------------------------------------------------------------
    // List pipe chain compilation (Phase 2: real DD for shopping_list)
    // -----------------------------------------------------------------------

    /// Check if a WHEN expression has SKIP arms and text-dependent body.
    ///
    /// Detects the pattern: `key_down.key |> WHEN { Enter => BLOCK { trimmed: input.text |> trim() ... }, __ => SKIP }`
    fn when_has_skip_and_text_dep(
        &self,
        from: &Spanned<Expression>,
        arms: &[static_expression::Arm],
    ) -> bool {
        // Check for SKIP arms
        let has_skip = arms.iter().any(|arm| {
            matches!(arm.body.node, Expression::Skip)
        });
        if !has_skip {
            return false;
        }
        // Check if from is a key_down event path
        if let Expression::Alias(Alias::WithoutPassed { parts, .. }) = &from.node {
            let path: String = parts.iter().map(|p| p.as_str()).collect::<Vec<_>>().join(".");
            let effective = if let Some(ref prefix) = self.scope_prefix {
                format!("{}.{}", prefix, path)
            } else {
                path
            };
            if effective.contains(".event.key_down") {
                return true;
            }
        }
        false
    }

    /// Compile a text-dependent WHEN pattern as HoldState + FlatMap.
    ///
    /// Handles: `key_down.key |> WHEN { Enter => BLOCK { trimmed: text|>trim(), ... }, __ => SKIP }`
    ///
    /// Creates:
    /// - TextChange input (tracks current text)
    /// - KeyDown input (triggers on key press)
    /// - HoldState combining both (text + last key → pending)
    /// - FlatMap extracting pending value
    fn compile_text_dependent_when(
        &mut self,
        name: &str,
        from: &Spanned<Expression>,
        _arms: &[static_expression::Arm],
    ) -> Result<VarId, String> {
        // Extract the event path from the `from` expression
        let from_path = match &from.node {
            Expression::Alias(Alias::WithoutPassed { parts, .. }) => {
                parts.iter().map(|p| p.as_str()).collect::<Vec<_>>().join(".")
            }
            _ => return Err("Expected alias for text-dependent WHEN source".to_string()),
        };
        let effective_from = if let Some(ref prefix) = self.scope_prefix {
            format!("{}.{}", prefix, from_path)
        } else {
            from_path
        };

        // Create KeyDown input (strip .key suffix for link_path)
        let (key_kind, key_link_path) = Self::detect_event_kind_and_path(&effective_from);
        let key_input_id = self.add_input(key_kind, Some(key_link_path.clone()));
        let key_input_var = self.fresh_var("key_down_input");
        self.collections.insert(key_input_var.clone(), CollectionSpec::Input(key_input_id));

        // Create TextChange input for the associated text input
        // Derive the change event path from the key_down path:
        // "store.X.event.key_down" → "store.X.event.change"
        let change_path = key_link_path.replace(".event.key_down", ".event.change");
        let text_input_id = self.add_input(InputKind::TextChange, Some(change_path));
        let text_input_var = self.fresh_var("text_change_input");
        self.collections.insert(text_input_var.clone(), CollectionSpec::Input(text_input_id));

        // Tag the text change events: Value::text(text) → [__t: "text", v: text]
        let tagged_text_var = self.fresh_var("tagged_text");
        self.collections.insert(
            tagged_text_var.clone(),
            CollectionSpec::Map {
                source: text_input_var,
                f: Arc::new(|v: &Value| {
                    Value::object([("__t", Value::text("text")), ("v", v.clone())])
                }),
            },
        );

        // Tag the key_down events: Value::text(key) → [__t: "key", v: key]
        let tagged_key_var = self.fresh_var("tagged_key");
        self.collections.insert(
            tagged_key_var.clone(),
            CollectionSpec::Map {
                source: key_input_var,
                f: Arc::new(|v: &Value| {
                    Value::object([("__t", Value::text("key")), ("v", v.clone())])
                }),
            },
        );

        // Concat tagged events
        let combined_var = self.fresh_var("text_key_events");
        self.collections.insert(
            combined_var.clone(),
            CollectionSpec::Concat(vec![tagged_text_var, tagged_key_var]),
        );

        // HoldState: tracks display text + pending text_to_add
        // State: [display: current_text, pending: Unit or trimmed_text]
        let initial_state = Value::object([
            ("display", Value::text("")),
            ("pending", Value::Unit),
        ]);
        let initial_var = self.fresh_var("text_state_init");
        self.collections.insert(initial_var.clone(), CollectionSpec::Literal(initial_state.clone()));

        let hold_var = self.fresh_var("text_key_state");
        self.collections.insert(
            hold_var.clone(),
            CollectionSpec::HoldState {
                initial: initial_var,
                events: combined_var,
                initial_value: initial_state,
                transform: Arc::new(|state: &Value, event: &Value| {
                    let event_type = event.get_field("__t")
                        .and_then(|v| v.as_text())
                        .unwrap_or("")
                        .to_string();
                    match event_type.as_str() {
                        "text" => {
                            // Text change: update display text, clear pending
                            let text = event.get_field("v").cloned().unwrap_or(Value::text(""));
                            Value::object([
                                ("display", text),
                                ("pending", Value::Unit),
                            ])
                        }
                        "key" => {
                            let key = event.get_field("v")
                                .and_then(|v| v.as_text())
                                .unwrap_or("")
                                .to_string();
                            if key == "Enter" {
                                let display = state.get_field("display")
                                    .and_then(|v| v.as_text())
                                    .unwrap_or("")
                                    .to_string();
                                let trimmed = display.trim().to_string();
                                if !trimmed.is_empty() {
                                    Value::object([
                                        ("display", Value::text("")),
                                        ("pending", Value::text(trimmed.as_str())),
                                    ])
                                } else {
                                    state.clone()
                                }
                            } else {
                                // Non-Enter key: clear pending (consumed by FlatMap)
                                let display = state.get_field("display").cloned().unwrap_or(Value::text(""));
                                Value::object([
                                    ("display", display),
                                    ("pending", Value::Unit),
                                ])
                            }
                        }
                        _ => state.clone(),
                    }
                }),
            },
        );

        // FlatMap: extract pending value when set
        let flatmap_var = VarId::new(name);
        self.collections.insert(
            flatmap_var.clone(),
            CollectionSpec::FlatMap {
                source: hold_var.clone(),
                f: Arc::new(|v: Value| {
                    let pending = v.get_field("pending").cloned().unwrap_or(Value::Unit);
                    if matches!(pending, Value::Unit) {
                        None
                    } else {
                        Some(pending)
                    }
                }),
            },
        );

        // Store the hold var so the document can access display text
        self.reactive_vars.insert(
            format!("{}.__state", name),
            hold_var,
        );

        self.reactive_vars.insert(name.to_string(), flatmap_var.clone());
        Ok(flatmap_var)
    }

    /// Compile `source |> List/count()`.
    ///
    /// Uses keyed DD operators when the source is a keyed collection:
    /// - Direct keyed source (e.g., `todos |> List/count()`) → direct ListCount
    /// - Keyed source through retain (e.g., `todos |> List/retain(...) |> List/count()`)
    ///   → ListCount wrapped in HoldState(initial=0) for empty-safety
    fn compile_list_count(
        &mut self,
        name: &str,
        from: &Spanned<Expression>,
    ) -> Result<VarId, String> {
        // Case 1: Direct keyed source (e.g., `todos |> List/count()`)
        // No wrapper needed — the keyed_hold always has initial items from LiteralList,
        // so ListCount fires at t=0 with the correct initial count.
        if let Some(keyed_var) = self.resolve_keyed_source(from) {
            let count_var = VarId::new(name);
            self.collections.insert(
                count_var.clone(),
                CollectionSpec::ListCount(keyed_var),
            );
            self.reactive_vars.insert(name.to_string(), count_var.clone());
            return Ok(count_var);
        }

        // Case 2: Keyed source through retain pipe
        // Pattern: `keyed_source |> List/retain(item, if: predicate) |> List/count()`
        // Uses HoldState(initial=0, events=ListCount) because ListRetain may produce
        // empty results, and ListCount on empty keyed collection never fires.
        if let Expression::Pipe { from: inner_from, to: inner_to } = &from.node {
            if let Expression::FunctionCall { path, arguments } = &inner_to.node {
                let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                if path_strs.as_slice() == ["List", "retain"] {
                    if let Some(keyed_var) = self.resolve_keyed_source(inner_from) {
                        let retain_var = self.compile_inline_keyed_retain(keyed_var, arguments)?;
                        return self.build_empty_safe_keyed_count(name, retain_var);
                    }
                }
            }
        }

        // Scalar fallback: compile source and use Map for counting
        let source_var = self.resolve_list_source(from)?;
        let count_var = VarId::new(name);
        self.collections.insert(
            count_var.clone(),
            CollectionSpec::Map {
                source: source_var,
                f: Arc::new(|v: &Value| Value::number(v.list_count() as f64)),
            },
        );
        self.reactive_vars.insert(name.to_string(), count_var.clone());
        Ok(count_var)
    }

    /// Build a keyed ListCount with HoldState for empty-safety.
    ///
    /// Creates: `HoldState(initial=Literal(0), events=ListCount(keyed_var))`.
    /// - At t=0: HoldState starts with initial_value=0
    /// - If ListCount fires (non-empty retain result): state updates to actual count
    /// - If ListCount doesn't fire (empty retain result): state stays at 0
    fn build_empty_safe_keyed_count(&mut self, name: &str, keyed_var: VarId) -> Result<VarId, String> {
        // ListCount on the keyed retain result
        let count_events_var = self.fresh_var(&format!("{}_count_events", name));
        self.collections.insert(
            count_events_var.clone(),
            CollectionSpec::ListCount(keyed_var),
        );

        // Literal(0) as initial value
        let zero_var = self.fresh_var(&format!("{}_zero", name));
        self.collections.insert(
            zero_var.clone(),
            CollectionSpec::Literal(Value::number(0.0)),
        );

        // HoldState: starts at 0, updates to ListCount result when available
        let count_var = VarId::new(name);
        self.collections.insert(
            count_var.clone(),
            CollectionSpec::HoldState {
                initial: zero_var,
                events: count_events_var,
                initial_value: Value::number(0.0),
                transform: Arc::new(|_state: &Value, event: &Value| event.clone()),
            },
        );
        self.reactive_vars.insert(name.to_string(), count_var.clone());
        Ok(count_var)
    }

    /// Compile a keyed ListRetain inline (not as a named variable).
    ///
    /// Used by compile_list_count for `keyed |> List/retain(...) |> List/count()`
    /// chains. Returns the keyed VarId of the retain result.
    fn compile_inline_keyed_retain(
        &mut self,
        keyed_var: VarId,
        arguments: &[Spanned<Argument>],
    ) -> Result<VarId, String> {
        let item_param: String = arguments.iter()
            .find(|a| a.node.value.is_none())
            .map(|a| a.node.name.as_str().to_string())
            .unwrap_or_else(|| "item".to_string());

        let predicate_expr = arguments.iter()
            .find(|a| a.node.name.as_str() == "if")
            .and_then(|a| a.node.value.as_ref())
            .ok_or_else(|| "List/retain missing 'if' argument".to_string())?;

        let reactive_deps = self.find_reactive_deps_in_expr(predicate_expr);

        let retain_var = self.fresh_var("keyed_retain");

        if reactive_deps.is_empty() {
            // Static predicate → CollectionSpec::ListRetain
            let compiler = self.compiler.clone();
            let pred_expr = predicate_expr.clone();
            let param_name = item_param;

            self.collections.insert(
                retain_var.clone(),
                CollectionSpec::ListRetain {
                    source: keyed_var,
                    predicate: Arc::new(move |item: &Value| {
                        let mut scope = indexmap::IndexMap::new();
                        scope.insert(param_name.clone(), item.clone());
                        compiler.eval_static_with_scope(&pred_expr, &scope)
                            .and_then(|v| Ok(v.as_bool().unwrap_or(false)))
                            .unwrap_or(false)
                    }),
                },
            );
        } else {
            // Reactive predicate → CollectionSpec::ListRetainReactive
            let reactive_dep = reactive_deps[0].clone();
            let reactive_var = self.reactive_vars.get(&reactive_dep)
                .cloned()
                .ok_or_else(|| format!("List/retain: reactive dep '{}' not found", reactive_dep))?;

            let compiler = self.compiler.clone();
            let pred_expr = predicate_expr.clone();
            let param_name = item_param;
            // Build the path parts for constructing __passed object.
            // reactive_dep = "store.selected_filter" → parts = ["store", "selected_filter"]
            let dep_parts: Vec<String> = reactive_dep.split('.').map(|s| s.to_string()).collect();

            // Wrap filter_state in HoldLatest with a default value.
            // The filter collection may not have data at epoch 0 (e.g., Router-derived).
            // HoldLatest ensures a default is available for the initial join.
            let filter_default = self.fresh_var("filter_default");
            self.collections.insert(
                filter_default.clone(),
                CollectionSpec::Literal(Value::tag("All")),
            );
            let filter_with_default = self.fresh_var("filter_with_default");
            self.collections.insert(
                filter_with_default.clone(),
                CollectionSpec::HoldLatest(vec![filter_default, reactive_var]),
            );

            self.collections.insert(
                retain_var.clone(),
                CollectionSpec::ListRetainReactive {
                    list: keyed_var,
                    filter_state: filter_with_default,
                    predicate: Arc::new(move |item: &Value, filter: &Value| {
                        let mut scope = indexmap::IndexMap::new();
                        scope.insert(param_name.clone(), item.clone());
                        // Build __passed object: { store: { selected_filter: filter } }
                        // The predicate expression uses PASSED.store.X which traverses __passed.
                        let mut passed_val = filter.clone();
                        for part in dep_parts.iter().rev() {
                            let mut fields = std::collections::BTreeMap::new();
                            fields.insert(Arc::from(part.as_str()), passed_val);
                            passed_val = Value::Object(Arc::new(fields));
                        }
                        scope.insert("__passed".to_string(), passed_val);
                        compiler.eval_static_with_scope(&pred_expr, &scope)
                            .and_then(|v| Ok(v.as_bool().unwrap_or(false)))
                            .unwrap_or(false)
                    }),
                },
            );
        }

        self.keyed_collection_vars.insert(retain_var.clone());
        Ok(retain_var)
    }

    /// Build the keyed display pipeline for a list.
    ///
    /// Scans function bodies for the pattern:
    ///   `PASSED.store.<list_name> |> List/retain(...) |> List/map(item, new: ...)`
    /// and compiles it as keyed DD operators:
    ///   `keyed_hold_var → ListRetainReactive → ListMapWithKey`
    ///
    /// The resulting display_var emits `(ListKey, Value)` pairs where each Value
    /// is a fully-evaluated element tree (ready for the bridge to render).
    fn build_display_pipeline(
        &mut self,
        list_name: &str,
        keyed_var: &VarId,
    ) -> Result<VarId, String> {
        // Find the display pipeline pattern in function bodies
        let pipeline = self.find_display_pipeline_in_functions(list_name)
            .ok_or_else(|| format!("No display pipeline found for '{}'", list_name))?;

        // Build keyed retain (if present) or use raw keyed var
        let map_source = if let Some(ref retain_args) = pipeline.retain_arguments {
            self.compile_inline_keyed_retain(keyed_var.clone(), retain_args)?
        } else {
            keyed_var.clone()
        };

        // Build ListMapWithKey (transform items to element Values + inject link paths)
        let display_var = self.fresh_var("display_pipeline");
        let compiler = self.compiler.clone();
        let map_new_expr = pipeline.map_new_expr.clone();
        let map_item_param = pipeline.map_item_param.clone();
        let list_path = format!("store.{}", list_name.strip_prefix("store.").unwrap_or(list_name));

        self.collections.insert(
            display_var.clone(),
            CollectionSpec::ListMapWithKey {
                source: map_source,
                f: Arc::new(move |key: &ListKey, item: &Value| {
                    // Inject link paths and hover state before evaluating the element template
                    let item_with_links = inject_item_link_paths_with_key(
                        item, &list_path, key.0.as_ref(),
                    );
                    let mut scope = IndexMap::new();
                    scope.insert(map_item_param.clone(), item_with_links);
                    // Use tolerant eval — the map function body may contain
                    // WHILE, WHEN, LINK patterns that need tolerance
                    compiler.eval_static_tolerant(&map_new_expr, &scope)
                }),
            },
        );
        self.keyed_collection_vars.insert(display_var.clone());
        Ok(display_var)
    }

    /// Scan function bodies for the display pipeline pattern on a keyed list.
    ///
    /// Looks for: `PASSED.store.<list_name> |> List/retain(...) |> List/map(item, new: ...)`
    /// in `items:` arguments of Element/stripe calls within function bodies.
    fn find_display_pipeline_in_functions(
        &self,
        list_name: &str,
    ) -> Option<DisplayPipelineInfo> {
        // The keyed name without "store." prefix for matching PASSED.store.<name>
        let short_name = list_name.strip_prefix("store.").unwrap_or(list_name);

        for (_fn_name, _params, body) in &self.compiler.functions {
            if let Some(info) = self.find_display_pipeline_in_expr(body, short_name) {
                return Some(info);
            }
        }
        None
    }

    /// Recursively search an expression for the display pipeline pattern.
    fn find_display_pipeline_in_expr(
        &self,
        expr: &Spanned<Expression>,
        list_short_name: &str,
    ) -> Option<DisplayPipelineInfo> {
        match &expr.node {
            // Check Element/stripe calls for items: argument with the pattern
            Expression::FunctionCall { path, arguments } => {
                let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                if matches!(path_strs.as_slice(), ["Element", "stripe"]) {
                    // Check the items: argument
                    if let Some(items_arg) = arguments.iter().find(|a| a.node.name.as_str() == "items") {
                        if let Some(ref items_expr) = items_arg.node.value {
                            if let Some(info) = self.match_display_pipeline_pattern(items_expr, list_short_name) {
                                return Some(info);
                            }
                        }
                    }
                }
                // Recurse into all argument values
                for arg in arguments {
                    if let Some(ref val_expr) = arg.node.value {
                        if let Some(info) = self.find_display_pipeline_in_expr(val_expr, list_short_name) {
                            return Some(info);
                        }
                    }
                }
            }
            Expression::Pipe { from, to } => {
                if let Some(info) = self.find_display_pipeline_in_expr(from, list_short_name) {
                    return Some(info);
                }
                if let Some(info) = self.find_display_pipeline_in_expr(to, list_short_name) {
                    return Some(info);
                }
            }
            Expression::Block { variables, output, .. } => {
                for var in variables {
                    if let Some(info) = self.find_display_pipeline_in_expr(&var.node.value, list_short_name) {
                        return Some(info);
                    }
                }
                if let Some(info) = self.find_display_pipeline_in_expr(output, list_short_name) {
                    return Some(info);
                }
            }
            Expression::While { arms } | Expression::When { arms } => {
                for arm in arms {
                    if let Some(info) = self.find_display_pipeline_in_expr(&arm.body, list_short_name) {
                        return Some(info);
                    }
                }
            }
            Expression::List { items } => {
                for item in items {
                    if let Some(info) = self.find_display_pipeline_in_expr(item, list_short_name) {
                        return Some(info);
                    }
                }
            }
            _ => {}
        }
        None
    }

    /// Match the specific pattern:
    ///   `PASSED.store.<list_name> |> List/retain(...) |> List/map(item, new: ...)`
    ///
    /// Returns the extracted pipeline info if the pattern matches.
    fn match_display_pipeline_pattern(
        &self,
        expr: &Spanned<Expression>,
        list_short_name: &str,
    ) -> Option<DisplayPipelineInfo> {
        if let Expression::Pipe { from: outer_from, to: outer_to } = &expr.node {
            // Check outer_to is List/map(...)
            if let Expression::FunctionCall { path: map_path, arguments: map_args } = &outer_to.node {
                let map_path_strs: Vec<&str> = map_path.iter().map(|s| s.as_str()).collect();
                if map_path_strs.as_slice() != ["List", "map"] {
                    return None;
                }

                // Extract map info (shared by both patterns)
                let map_item_param = map_args.iter()
                    .find(|a| a.node.value.is_none())
                    .map(|a| a.node.name.as_str().to_string())
                    .unwrap_or_else(|| "item".to_string());

                let map_new_expr = map_args.iter()
                    .find(|a| a.node.name.as_str() == "new")
                    .and_then(|a| a.node.value.as_ref())?
                    .clone();

                // Pattern 1: PASSED.store.<name> |> List/retain(...) |> List/map(...)
                if let Expression::Pipe { from: inner_from, to: inner_to } = &outer_from.node {
                    if self.is_passed_store_alias(inner_from, list_short_name) {
                        if let Expression::FunctionCall { path: retain_path, arguments: retain_args } = &inner_to.node {
                            let retain_path_strs: Vec<&str> = retain_path.iter().map(|s| s.as_str()).collect();
                            if retain_path_strs.as_slice() == ["List", "retain"] {
                                return Some(DisplayPipelineInfo {
                                    retain_arguments: Some(retain_args.clone()),
                                    map_item_param,
                                    map_new_expr,
                                });
                            }
                        }
                    }
                }

                // Pattern 2: PASSED.store.<name> |> List/map(...) (no retain)
                if self.is_passed_store_alias(outer_from, list_short_name) {
                    return Some(DisplayPipelineInfo {
                        retain_arguments: None,
                        map_item_param,
                        map_new_expr,
                    });
                }
            }
        }
        None
    }

    /// Check if an expression is `PASSED.store.<name>`.
    fn is_passed_store_alias(&self, expr: &Spanned<Expression>, expected_name: &str) -> bool {
        if let Expression::Alias(Alias::WithPassed { extra_parts }) = &expr.node {
            let parts: Vec<&str> = extra_parts.iter().map(|s| s.as_str()).collect();
            return parts == ["store", expected_name];
        }
        false
    }

    /// Compile `source |> List/retain(item, if: predicate)`.
    ///
    /// If the predicate is purely static (depends only on the list item),
    /// this becomes a Map that filters items.
    /// If the predicate depends on a reactive variable, this becomes a Join
    /// of the list collection with the reactive variable, then a Map that filters.
    fn compile_list_retain(
        &mut self,
        name: &str,
        from: &Spanned<Expression>,
        arguments: &[Spanned<Argument>],
    ) -> Result<VarId, String> {
        // Extract the item parameter name (e.g., "n" from `List/retain(n, if: ...)`)
        let item_param: String = arguments.iter()
            .find(|a| a.node.value.is_none())
            .map(|a| a.node.name.as_str().to_string())
            .unwrap_or_else(|| "item".to_string());

        // Extract the `if:` predicate expression
        let predicate_expr = arguments.iter()
            .find(|a| a.node.name.as_str() == "if")
            .and_then(|a| a.node.value.as_ref())
            .ok_or_else(|| "List/retain missing 'if' argument".to_string())?;

        // Check if predicate depends on any reactive variables
        let reactive_deps = self.find_reactive_deps_in_expr(predicate_expr);

        // Use scalar approach on assembled list.
        // Keyed ListRetain/ListRetainReactive will be used later for the bridge
        // display pipeline (Task 5). For now, retain→count chains must stay scalar
        // because keyed ListRetain on empty results produces no diffs, which would
        // block downstream ListCount from emitting.
        let source_var = self.resolve_list_source(from)?;

        if reactive_deps.is_empty() {
            // Static predicate: simple Map that filters items
            let compiler = self.compiler.clone();
            let pred_expr = predicate_expr.clone();
            let param_name = item_param.clone();
            let retain_var = VarId::new(name);
            self.collections.insert(
                retain_var.clone(),
                CollectionSpec::Map {
                    source: source_var,
                    f: Arc::new(move |list: &Value| {
                        list.list_retain(|item| {
                            let mut scope = indexmap::IndexMap::new();
                            scope.insert(param_name.clone(), item.clone());
                            let result = compiler.eval_static_with_scope(&pred_expr, &scope)
                                .unwrap_or(Value::Unit);
                            result.as_bool().unwrap_or(false)
                        })
                    }),
                },
            );
            self.reactive_vars.insert(name.to_string(), retain_var.clone());
            Ok(retain_var)
        } else {
            // Reactive predicate: Join source list with reactive deps, then filter
            // For simplicity, support a single reactive dependency for now
            let reactive_dep = reactive_deps[0].clone();
            let reactive_var = self.reactive_vars.get(&reactive_dep)
                .cloned()
                .ok_or_else(|| format!("List/retain: reactive dep '{}' not found", reactive_dep))?;

            // Join list × reactive_state → Map(filter)
            let join_var = self.fresh_var(&format!("{}_join", name));
            self.collections.insert(
                join_var.clone(),
                CollectionSpec::Join {
                    left: source_var.clone(),
                    right: reactive_var.clone(),
                    combine: Arc::new(|list: &Value, state: &Value| {
                        Value::object([("list", list.clone()), ("state", state.clone())])
                    }),
                },
            );

            let compiler = self.compiler.clone();
            let pred_expr = predicate_expr.clone();
            let param_name = item_param.clone();
            let dep_name = reactive_dep.clone();
            // Strip scope prefix from dep_name for scope lookup
            let dep_short = dep_name.rsplit('.').next().unwrap_or(&dep_name).to_string();
            let retain_var = VarId::new(name);
            self.collections.insert(
                retain_var.clone(),
                CollectionSpec::Map {
                    source: join_var,
                    f: Arc::new(move |combined: &Value| {
                        let list = combined.get_field("list").cloned().unwrap_or(Value::Unit);
                        let state = combined.get_field("state").cloned().unwrap_or(Value::Unit);
                        list.list_retain(|item| {
                            let mut scope = indexmap::IndexMap::new();
                            scope.insert(param_name.clone(), item.clone());
                            scope.insert(dep_short.clone(), state.clone());
                            let result = compiler.eval_static_with_scope(&pred_expr, &scope)
                                .unwrap_or(Value::Unit);
                            result.as_bool().unwrap_or(false)
                        })
                    }),
                },
            );
            self.reactive_vars.insert(name.to_string(), retain_var.clone());
            Ok(retain_var)
        }
    }

    /// Compile `source |> List/map(item, new: transform)` as a Map on a scalar Value::List.
    fn compile_list_map(
        &mut self,
        name: &str,
        from: &Spanned<Expression>,
        arguments: &[Spanned<Argument>],
    ) -> Result<VarId, String> {
        let source_var = self.resolve_list_source(from)?;

        // Extract item param name and `new:` expression
        let item_param: String = arguments.iter()
            .find(|a| a.node.value.is_none())
            .map(|a| a.node.name.as_str().to_string())
            .unwrap_or_else(|| "item".to_string());

        let new_expr = arguments.iter()
            .find(|a| a.node.name.as_str() == "new")
            .and_then(|a| a.node.value.as_ref())
            .ok_or_else(|| "List/map missing 'new' argument".to_string())?;

        let compiler = self.compiler.clone();
        let new_expr = new_expr.clone();
        let param_name = item_param.clone();
        let map_var = VarId::new(name);
        self.collections.insert(
            map_var.clone(),
            CollectionSpec::Map {
                source: source_var,
                f: Arc::new(move |list: &Value| {
                    list.list_map(|item| {
                        let mut scope = indexmap::IndexMap::new();
                        scope.insert(param_name.clone(), item.clone());
                        compiler.eval_static_with_scope(&new_expr, &scope)
                            .unwrap_or(Value::Unit)
                    })
                }),
            },
        );
        self.reactive_vars.insert(name.to_string(), map_var.clone());
        Ok(map_var)
    }

    /// Compile `source |> List/remove(item, on: condition)`.
    ///
    /// Walks the pipe chain to collect ALL list operations (append, remove, clear)
    /// and builds a single HoldState that handles all event types.
    fn compile_list_remove(
        &mut self,
        name: &str,
        from: &Spanned<Expression>,
        arguments: &[Spanned<Argument>],
    ) -> Result<VarId, String> {
        // Collect all operations in the pipe chain
        let mut ops = Vec::new();
        ops.push(ListChainOp::Remove(arguments));
        let initial_list_expr = self.collect_list_chain_ops(from, &mut ops)?;

        self.build_unified_list_holdstate(name, initial_list_expr, &ops)
    }

    /// Walk a pipe chain collecting list operations (append, remove, clear).
    /// Returns the base expression (the initial LIST { ... } literal).
    fn collect_list_chain_ops<'b>(
        &self,
        expr: &'b Spanned<Expression>,
        ops: &mut Vec<ListChainOp<'b>>,
    ) -> Result<&'b Spanned<Expression>, String> {
        match &expr.node {
            Expression::Pipe { from, to } => {
                match &to.node {
                    Expression::FunctionCall { path, arguments } => {
                        let p: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                        match p.as_slice() {
                            ["List", "append"] => {
                                ops.push(ListChainOp::Append(arguments));
                                self.collect_list_chain_ops(from, ops)
                            }
                            ["List", "remove"] => {
                                ops.push(ListChainOp::Remove(arguments));
                                self.collect_list_chain_ops(from, ops)
                            }
                            ["List", "clear"] => {
                                ops.push(ListChainOp::Clear(arguments));
                                self.collect_list_chain_ops(from, ops)
                            }
                            _ => Ok(expr),
                        }
                    }
                    _ => Ok(expr),
                }
            }
            _ => Ok(expr),
        }
    }

    /// Build a unified HoldState for a full list operation chain.
    ///
    /// Handles any combination of append, remove, and clear operations
    /// by creating tagged event streams and a single HoldState transform.
    fn build_unified_list_holdstate(
        &mut self,
        name: &str,
        initial_list_expr: &Spanned<Expression>,
        ops: &[ListChainOp],
    ) -> Result<VarId, String> {
        // Pre-check: if any Remove op references "item", use keyed pipeline
        let has_wildcard_ops = ops.iter().rev().any(|op| {
            if let ListChainOp::Remove(arguments) = op {
                let on_arg = arguments.iter()
                    .find(|a| a.node.name.as_str() == "on")
                    .and_then(|a| a.node.value.as_ref());
                if let Some(on_expr) = on_arg {
                    return Self::expr_references_name(on_expr, "item");
                }
            }
            false
        });

        if has_wildcard_ops {
            return self.build_keyed_list_holdstate(name, initial_list_expr, ops);
        }

        let mut event_vars = Vec::new();
        let mut remove_counter = 0u32;

        // Evaluate initial list statically
        let initial_list = self.compiler.eval_static(initial_list_expr)
            .unwrap_or_else(|_| Value::empty_list());

        // Process each operation (they were collected outermost-first, so reverse to get pipeline order)
        for op in ops.iter().rev() {
            match op {
                ListChainOp::Append(arguments) => {
                    let item_arg = arguments.iter()
                        .find(|a| a.node.name.as_str() == "item")
                        .and_then(|a| a.node.value.as_ref())
                        .ok_or_else(|| "List/append missing 'item' argument".to_string())?;
                    let item_source = self.resolve_reactive_source(item_arg)?;

                    let append_then_var = self.fresh_var("append_event");
                    self.collections.insert(
                        append_then_var.clone(),
                        CollectionSpec::Then {
                            source: item_source,
                            body: Arc::new(|v: &Value| {
                                Value::object([("__t", Value::text("append")), ("v", v.clone())])
                            }),
                        },
                    );
                    event_vars.push(append_then_var);
                }

                ListChainOp::Remove(arguments) => {
                    // Only global event remove reaches here — per-item remove
                    // (referencing `item`) is handled by build_keyed_list_holdstate.
                    let on_arg = arguments.iter()
                        .find(|a| a.node.name.as_str() == "on")
                        .and_then(|a| a.node.value.as_ref())
                        .ok_or_else(|| "List/remove missing 'on' argument".to_string())?;

                    let (event_var, _) = self.compile_event_source(on_arg)?;
                    let remove_tag = format!("remove_{}", remove_counter);
                    remove_counter += 1;
                    let tag = remove_tag.clone();
                    let tagged_remove = self.fresh_var("tagged_remove");
                    self.collections.insert(
                        tagged_remove.clone(),
                        CollectionSpec::Then {
                            source: event_var,
                            body: Arc::new(move |_v: &Value| {
                                Value::object([("__t", Value::text(tag.as_str()))])
                            }),
                        },
                    );
                    event_vars.push(tagged_remove);
                }

                ListChainOp::Clear(arguments) => {
                    let clear_source_expr = arguments.iter()
                        .find(|a| a.node.name.as_str() == "on")
                        .and_then(|a| a.node.value.as_ref())
                        .ok_or_else(|| "List/clear missing 'on' argument".to_string())?;
                    let (clear_input_var, _) = self.compile_event_source(clear_source_expr)?;

                    let clear_then_var = self.fresh_var("clear_event");
                    self.collections.insert(
                        clear_then_var.clone(),
                        CollectionSpec::Then {
                            source: clear_input_var,
                            body: Arc::new(|_v: &Value| {
                                Value::object([("__t", Value::text("clear"))])
                            }),
                        },
                    );
                    event_vars.push(clear_then_var);
                }
            }
        }

        // Concat all events
        let events_var = if event_vars.len() == 1 {
            event_vars.into_iter().next().unwrap()
        } else if event_vars.is_empty() {
            let empty_var = self.fresh_var("no_events");
            self.collections.insert(empty_var.clone(), CollectionSpec::Literal(Value::Unit));
            empty_var
        } else {
            let concat_var = self.fresh_var("list_events");
            self.collections.insert(concat_var.clone(), CollectionSpec::Concat(event_vars));
            concat_var
        };

        // HoldState: maintains the items list
        let initial_var = self.fresh_var("list_init");
        let effective_initial = self.persisted_holds
            .get(name)
            .cloned()
            .unwrap_or_else(|| initial_list.clone());
        self.collections.insert(initial_var.clone(), CollectionSpec::Literal(effective_initial.clone()));

        // Build transform that handles all event types
        let list_var = VarId::new(name);
        self.collections.insert(
            list_var.clone(),
            CollectionSpec::HoldState {
                initial: initial_var,
                events: events_var,
                initial_value: effective_initial,
                transform: Arc::new(|state: &Value, event: &Value| {
                    let event_type = event.get_field("__t")
                        .and_then(|v| v.as_text())
                        .unwrap_or("")
                        .to_string();
                    match event_type.as_str() {
                        "append" => {
                            let item = event.get_field("v").cloned().unwrap_or(Value::Unit);
                            let count = state.list_count();
                            state.list_append(item, count)
                        }
                        "clear" => Value::empty_list(),
                        _ => state.clone(),
                    }
                }),
            },
        );

        self.reactive_vars.insert(name.to_string(), list_var.clone());

        // Emit persistence side effect if storage_key is set
        if let Some(key) = self.storage_key.clone() {
            let persist_var = self.fresh_var(&format!("{}_persist", name));
            self.collections.insert(
                persist_var,
                CollectionSpec::SideEffect {
                    source: list_var.clone(),
                    effect: SideEffectKind::PersistHold {
                        key,
                        hold_name: name.to_string(),
                    },
                },
            );
        }

        Ok(list_var)
    }

    /// Build a keyed list pipeline for lists with per-item events (wildcard).
    ///
    /// Instead of a monolithic HoldState holding the entire list, this creates:
    /// LiteralList → AppendNewKeyed → ListAppend → KeyedHoldState → AssembleList
    /// with MapToKeyed for wildcard event demuxing and per-item transform.
    fn build_keyed_list_holdstate(
        &mut self,
        name: &str,
        initial_list_expr: &Spanned<Expression>,
        ops: &[ListChainOp],
    ) -> Result<VarId, String> {
        use super::types::ClassifyFn;

        // Evaluate initial list statically
        let initial_list = self.compiler.eval_static(initial_list_expr)
            .unwrap_or_else(|_| Value::empty_list());

        // Get effective initial (with persistence)
        let effective_initial = self.persisted_holds
            .get(name)
            .cloned()
            .unwrap_or_else(|| initial_list.clone());

        // Decompose initial list into (ListKey, Value) pairs
        let initial_items: Vec<(ListKey, Value)> = if let Value::Tagged { ref tag, ref fields } = effective_initial {
            if tag.as_ref() == "List" {
                fields.iter().map(|(k, v)| (ListKey::new(k.as_ref()), v.clone())).collect()
            } else { Vec::new() }
        } else { Vec::new() };
        let initial_counter = initial_items.len();

        // 1. LiteralList for initial items
        let initial_list_var = self.fresh_var("keyed_initial");
        self.collections.insert(initial_list_var.clone(), CollectionSpec::LiteralList(initial_items));

        // 2. Find append source from ops and create AppendNewKeyed
        let mut append_source: Option<VarId> = None;
        for op in ops.iter().rev() {
            if let ListChainOp::Append(arguments) = op {
                let item_arg = arguments.iter()
                    .find(|a| a.node.name.as_str() == "item")
                    .and_then(|a| a.node.value.as_ref())
                    .ok_or_else(|| "List/append missing 'item' argument".to_string())?;
                append_source = Some(self.resolve_reactive_source(item_arg)?);
                break;
            }
        }

        // 3. Create membership collection (initial + appended)
        let membership_var = if let Some(src) = append_source {
            let append_keyed_var = self.fresh_var("append_keyed");
            self.collections.insert(
                append_keyed_var.clone(),
                CollectionSpec::AppendNewKeyed {
                    source: src,
                    f: Arc::new(|v: &Value| v.clone()),
                    initial_counter,
                },
            );

            let membership = self.fresh_var("membership");
            self.collections.insert(
                membership.clone(),
                CollectionSpec::ListAppend {
                    list: initial_list_var.clone(),
                    new_items: append_keyed_var,
                },
            );
            membership
        } else {
            initial_list_var.clone()
        };

        // 4. Register wildcard input and create MapToKeyed
        let wildcard_id = self.add_input(InputKind::LinkPress, Some("__wildcard".to_string()));
        let wildcard_input_var = self.fresh_var("wildcard_input");
        self.collections.insert(wildcard_input_var.clone(), CollectionSpec::Input(wildcard_id));

        let classify: ClassifyFn = Arc::new(|v: &Value| {
            let path = v.get_field("path")?.as_text()?.to_string();
            let event_value = v.get_field("value").cloned().unwrap_or(Value::Unit);

            let key = extract_list_item_key_from_path(&path)?;

            // Find subpath after the key
            let subpath = path.split('.')
                .skip_while(|p| !(p.len() == 4 && p.chars().all(|c| c.is_ascii_digit())))
                .skip(1) // skip the key itself
                .collect::<Vec<_>>()
                .join(".");

            let event_tag = event_value.as_tag().unwrap_or("");

            let classified = if subpath.contains("remove_todo_button") && event_tag == "Press" {
                Value::object([("type", Value::text("remove"))])
            } else if subpath.contains("todo_checkbox") && event_tag == "Click" {
                Value::object([("type", Value::text("checkbox_click"))])
            } else if subpath.contains("todo_title_element") && event_tag == "DoubleClick" {
                Value::object([("type", Value::text("double_click"))])
            } else if subpath.contains("editing_todo_title_element") {
                if let Some(text) = event_value.as_text() {
                    match text {
                        "Enter" => Value::object([("type", Value::text("edit_key_enter"))]),
                        "Escape" => Value::object([("type", Value::text("edit_key_escape"))]),
                        _ => Value::object([
                            ("type", Value::text("edit_text_change")),
                            ("text", Value::text(text)),
                        ]),
                    }
                } else if event_tag == "Blur" {
                    Value::object([("type", Value::text("edit_blur"))])
                } else {
                    return None;
                }
            } else if subpath.ends_with("hovered") || subpath.contains(".hovered") {
                let hovered = matches!(event_value, Value::Bool(true));
                Value::object([
                    ("type", Value::text("hover_change")),
                    ("hovered", Value::bool(hovered)),
                ])
            } else if subpath == "toggle_all" {
                // Synthetic per-item event from IO layer
                let target = event_value.as_bool().unwrap_or(false);
                Value::object([
                    ("type", Value::text("toggle_all")),
                    ("target", Value::bool(target)),
                ])
            } else if subpath == "remove_if_completed" {
                Value::object([("type", Value::text("remove_if_completed"))])
            } else {
                return None;
            };

            Some((ListKey::new(key), classified))
        });

        let wildcard_keyed_var = self.fresh_var("wildcard_keyed");
        self.collections.insert(
            wildcard_keyed_var.clone(),
            CollectionSpec::MapToKeyed {
                source: wildcard_input_var,
                classify,
            },
        );

        // 5. Detect broadcast sources (toggle_all, remove_completed)
        let mut broadcast_vars: Vec<VarId> = Vec::new();
        let mut has_broadcasts = false;

        // Toggle-all: check if the store has a toggle_all_checkbox LINK
        if let Some(toggle_expr) = self.compiler.get_var_expr("store.elements.toggle_all_checkbox") {
            if matches!(toggle_expr.node, Expression::Link) {
                let toggle_path = "store.elements.toggle_all_checkbox".to_string();
                let input_id = self.add_input(InputKind::LinkClick, Some(toggle_path));
                let toggle_input_var = self.fresh_var("toggle_all_input");
                self.collections.insert(toggle_input_var.clone(), CollectionSpec::Input(input_id));
                // THEN: produce a tagged broadcast event
                let toggle_then_var = self.fresh_var("toggle_all_then");
                self.collections.insert(toggle_then_var.clone(), CollectionSpec::Then {
                    source: toggle_input_var,
                    body: Arc::new(|_| Value::tag("toggle_all")),
                });
                broadcast_vars.push(toggle_then_var);
                has_broadcasts = true;
            }
        }

        // Remove-completed: detect from ops
        for op in ops.iter().rev() {
            if let ListChainOp::Remove(arguments) = op {
                let on_arg = arguments.iter()
                    .find(|a| a.node.name.as_str() == "on")
                    .and_then(|a| a.node.value.as_ref());
                if let Some(on_expr) = on_arg {
                    if Self::expr_references_name(on_expr, "item") {
                        if let Expression::Pipe { from, to } = &on_expr.node {
                            if matches!(&to.node, Expression::Then { .. }) {
                                if !Self::expr_references_name(from, "item") {
                                    let (event_var, _) = self.compile_event_source(from)?;
                                    let remove_then_var = self.fresh_var("remove_completed_then");
                                    self.collections.insert(remove_then_var.clone(), CollectionSpec::Then {
                                        source: event_var,
                                        body: Arc::new(|_| Value::tag("remove_completed")),
                                    });
                                    broadcast_vars.push(remove_then_var);
                                    has_broadcasts = true;
                                }
                            }
                        }
                    }
                }
            }
        }

        // Build broadcast VarId (concat if multiple sources)
        let broadcasts_var: Option<VarId> = if has_broadcasts {
            if broadcast_vars.len() == 1 {
                Some(broadcast_vars.pop().unwrap())
            } else {
                let concat_var = self.fresh_var("broadcasts_concat");
                self.collections.insert(concat_var.clone(), CollectionSpec::Concat(broadcast_vars));
                Some(concat_var)
            }
        } else {
            None
        };

        // Build broadcast handler
        let broadcast_handler: Option<BroadcastHandlerFn> = if has_broadcasts {
            Some(Arc::new(|states: &std::collections::HashMap<ListKey, Value>, event: &Value| {
                let event_tag = event.as_tag().unwrap_or("");
                match event_tag {
                    "toggle_all" => {
                        let all_completed = !states.is_empty() && states.values().all(|item| {
                            item.get_field("completed")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false)
                        });
                        let target = !all_completed;
                        let target_val = if target { Value::tag("True") } else { Value::tag("False") };
                        states.iter().map(|(key, item)| {
                            let new_item = item.update_field("completed", target_val.clone());
                            (key.clone(), Some(new_item))
                        }).collect()
                    }
                    "remove_completed" => {
                        states.iter().filter_map(|(key, item)| {
                            let completed = item.get_field("completed")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                            if completed { Some((key.clone(), None)) } else { None }
                        }).collect()
                    }
                    _ => Vec::new(),
                }
            }))
        } else {
            None
        };

        // 6. KeyedHoldState: per-item state with self-removal sentinel + broadcast support
        let keyed_hold_var = self.fresh_var("keyed_hold");
        // Store the keyed var for downstream keyed operators (ListCount, ListRetain)
        self.keyed_hold_vars.insert(name.to_string(), keyed_hold_var.clone());
        self.keyed_collection_vars.insert(keyed_hold_var.clone());
        self.collections.insert(
            keyed_hold_var.clone(),
            CollectionSpec::KeyedHoldState {
                initial: membership_var,
                events: wildcard_keyed_var,
                transform: Arc::new(|state: &Value, event: &Value| {
                    let event_type = event.get_field("type")
                        .and_then(|v| v.as_text())
                        .unwrap_or("")
                        .to_string();

                    match event_type.as_str() {
                        "remove" => Value::Unit, // Self-removal sentinel
                        "checkbox_click" => {
                            let completed = state.get_field("completed")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                            state.update_field(
                                "completed",
                                if !completed { Value::tag("True") } else { Value::tag("False") },
                            )
                        }
                        "double_click" => {
                            let title = state.get_field("title")
                                .and_then(|v| v.as_text())
                                .unwrap_or("")
                                .to_string();
                            state
                                .update_field("editing", Value::tag("True"))
                                .update_field("__edit_text", Value::text(title.as_str()))
                        }
                        "edit_key_enter" => {
                            let edit_text = state.get_field("__edit_text")
                                .and_then(|v| v.as_text())
                                .unwrap_or("")
                                .to_string();
                            let trimmed = edit_text.trim();
                            if !trimmed.is_empty() {
                                state
                                    .update_field("title", Value::text(trimmed))
                                    .update_field("editing", Value::tag("False"))
                            } else {
                                state.clone()
                            }
                        }
                        "edit_key_escape" => {
                            state.update_field("editing", Value::tag("False"))
                        }
                        "edit_text_change" => {
                            let text = event.get_field("text")
                                .and_then(|v| v.as_text())
                                .unwrap_or("");
                            state.update_field("__edit_text", Value::text(text))
                        }
                        "edit_blur" => {
                            let edit_text = state.get_field("__edit_text")
                                .and_then(|v| v.as_text())
                                .unwrap_or("")
                                .to_string();
                            let trimmed = edit_text.trim();
                            if !trimmed.is_empty() {
                                state
                                    .update_field("title", Value::text(trimmed))
                                    .update_field("editing", Value::tag("False"))
                            } else {
                                state.update_field("editing", Value::tag("False"))
                            }
                        }
                        "hover_change" => {
                            let hovered = event.get_field("hovered")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                            state.update_field(
                                "__hovered",
                                if hovered { Value::tag("True") } else { Value::tag("False") },
                            )
                        }
                        _ => state.clone(),
                    }
                }),
                broadcasts: broadcasts_var,
                broadcast_handler,
            },
        );

        // 7. Stub list for document closure (Phase 2).
        // Items render via keyed diffs from the display pipeline — O(1) per item change.
        // The document closure only needs the list for structural checks like
        // List/is_empty(). We derive a "stub list" from ListCount on the keyed hold:
        //   keyed_hold → ListCount → Map(count_to_stub_list) → reactive_var
        // This is O(1) per add/remove (count changes), NOT O(N) per item change.
        let count_var = self.fresh_var(&format!("{}_count_for_stub", name));
        self.collections.insert(
            count_var.clone(),
            CollectionSpec::ListCount(keyed_hold_var),
        );
        let list_var = VarId::new(name);
        self.collections.insert(
            list_var.clone(),
            CollectionSpec::Map {
                source: count_var,
                f: Arc::new(|count_val: &Value| {
                    let count = count_val.as_number().unwrap_or(0.0) as usize;
                    if count == 0 {
                        Value::empty_list()
                    } else {
                        // Stub list with N placeholder items — just enough for
                        // List/is_empty() and List/count() to return correct results.
                        // Actual item rendering flows via keyed diffs.
                        let mut fields = std::collections::BTreeMap::new();
                        for i in 0..count {
                            fields.insert(
                                Arc::from(format!("{:04}", i).as_str()),
                                Value::Unit,
                            );
                        }
                        Value::Tagged {
                            tag: Arc::from("List"),
                            fields: Arc::new(fields),
                        }
                    }
                }),
            },
        );
        self.reactive_vars.insert(name.to_string(), list_var.clone());

        // 8. Persistence handled by keyed diff path in worker.rs
        // (KeyedPersistenceState + save_keyed_list). No SideEffect needed.

        Ok(list_var)
    }

    /// Find the link path associated with an input var.
    fn find_link_path_for_input(&self, var_id: &VarId) -> Option<String> {
        // Walk back to the Input spec to find the link path
        if let Some(CollectionSpec::Input(input_id)) = self.collections.get(var_id) {
            for spec in &self.inputs {
                if spec.id == *input_id {
                    return spec.link_path.clone();
                }
            }
        }
        // Check if it's a Then wrapping an Input
        if let Some(CollectionSpec::Then { source, .. }) = self.collections.get(var_id) {
            return self.find_link_path_for_input(source);
        }
        None
    }

    /// Analyze a List/remove `on:` condition that references `item`.
    /// Returns (global_event_var, remove_tag) if there's a global trigger with per-item condition.
    /// Returns None if it's a pure per-item event (handled entirely by wildcard).
    /// Check if an expression references a name (e.g., "item").
    fn expr_references_name(expr: &Spanned<Expression>, name: &str) -> bool {
        match &expr.node {
            Expression::Alias(Alias::WithoutPassed { parts, .. }) => {
                parts.first().map(|p| p.as_str() == name).unwrap_or(false)
            }
            Expression::Pipe { from, to } => {
                Self::expr_references_name(from, name) || Self::expr_references_name(to, name)
            }
            Expression::Then { body } => Self::expr_references_name(body, name),
            Expression::When { arms } => {
                arms.iter().any(|a| Self::expr_references_name(&a.body, name))
            }
            Expression::While { arms } => {
                arms.iter().any(|a| Self::expr_references_name(&a.body, name))
            }
            Expression::FunctionCall { arguments, .. } => {
                arguments.iter().any(|a| {
                    a.node.value.as_ref()
                        .map(|v| Self::expr_references_name(v, name))
                        .unwrap_or(false)
                })
            }
            Expression::Block { variables, output } => {
                variables.iter().any(|v| Self::expr_references_name(&v.node.value, name))
                    || Self::expr_references_name(output, name)
            }
            _ => false,
        }
    }

    /// Resolve a list source expression to a VarId.
    /// The source could be a reactive variable or a static expression.
    fn resolve_list_source(
        &mut self,
        expr: &Spanned<Expression>,
    ) -> Result<VarId, String> {
        // Try to resolve as a reactive variable first
        if let Ok(var) = self.resolve_reactive_source(expr) {
            return Ok(var);
        }

        // Try with scope prefix (e.g., alias "numbers" → "store.numbers")
        if let Expression::Alias(Alias::WithoutPassed { parts, .. }) = &expr.node {
            if parts.len() == 1 {
                let name = parts[0].as_str();
                if let Some(ref prefix) = self.scope_prefix {
                    let full_name = format!("{}.{}", prefix, name);
                    if let Some(var_id) = self.reactive_vars.get(&full_name) {
                        return Ok(var_id.clone());
                    }
                    // Try static evaluation of the prefixed variable
                    if let Some(var_expr) = self.compiler.get_var_expr(&full_name) {
                        let var_expr = var_expr.clone();
                        if let Ok(value) = self.compiler.eval_static_with_scope(&var_expr, &indexmap::IndexMap::new()) {
                            let literal_var = self.fresh_var("static_list");
                            self.collections.insert(literal_var.clone(), CollectionSpec::Literal(value));
                            return Ok(literal_var);
                        }
                    }
                }
            }
        }

        // Try to evaluate statically and create a Literal
        if let Ok(value) = self.compiler.eval_static_with_scope(expr, &indexmap::IndexMap::new()) {
            let literal_var = self.fresh_var("static_list");
            self.collections.insert(literal_var.clone(), CollectionSpec::Literal(value));
            Ok(literal_var)
        } else {
            Err(format!("Could not resolve list source: {:?}", std::mem::discriminant(&expr.node)))
        }
    }

    /// Try to resolve a source expression as a keyed collection.
    ///
    /// Checks both `keyed_hold_vars` (initial keyed sources) and
    /// `keyed_collection_vars` (results of keyed operations like ListRetain).
    /// Only matches direct alias references (not Pipe expressions).
    /// Handles scope prefix resolution (e.g., `todos` → `store.todos`).
    fn resolve_keyed_source(&self, expr: &Spanned<Expression>) -> Option<VarId> {
        if let Expression::Alias(Alias::WithoutPassed { parts, .. }) = &expr.node {
            let name = parts.iter().map(|p| p.as_str()).collect::<Vec<_>>().join(".");
            // Direct lookup in keyed_hold_vars
            if let Some(var) = self.keyed_hold_vars.get(&name) {
                return Some(var.clone());
            }
            // Check keyed_collection_vars
            let var_id = VarId::new(name.as_str());
            if self.keyed_collection_vars.contains(&var_id) {
                return Some(var_id);
            }
            // Try with scope prefix (e.g., "todos" → "store.todos")
            if parts.len() == 1 {
                if let Some(ref prefix) = self.scope_prefix {
                    let full_name = format!("{}.{}", prefix, name);
                    if let Some(var) = self.keyed_hold_vars.get(&full_name) {
                        return Some(var.clone());
                    }
                    let full_var_id = VarId::new(full_name.as_str());
                    if self.keyed_collection_vars.contains(&full_var_id) {
                        return Some(full_var_id);
                    }
                }
            }
        }
        None
    }

    /// Find reactive variable names referenced in an expression.
    fn find_reactive_deps_in_expr(&self, expr: &Spanned<Expression>) -> Vec<String> {
        let mut deps = Vec::new();
        self.collect_reactive_deps(expr, &mut deps);
        deps
    }

    fn collect_reactive_deps(&self, expr: &Spanned<Expression>, deps: &mut Vec<String>) {
        match &expr.node {
            Expression::Alias(alias) => {
                // Build the full alias path and check if it (or a prefix) is a reactive var
                let path = match alias {
                    Alias::WithoutPassed { parts, .. } => {
                        parts.iter().map(|p| p.as_str()).collect::<Vec<_>>().join(".")
                    }
                    Alias::WithPassed { extra_parts } => {
                        let mut p = vec!["PASSED"];
                        p.extend(extra_parts.iter().map(|s| s.as_str()));
                        p.join(".")
                    }
                };
                // Check with scope prefix
                let full_path = if let Some(ref prefix) = self.scope_prefix {
                    format!("{}.{}", prefix, path)
                } else {
                    path.clone()
                };
                if self.reactive_vars.contains_key(&full_path) {
                    deps.push(full_path);
                } else if self.reactive_vars.contains_key(&path) {
                    deps.push(path);
                } else if let Some(without_passed) = path.strip_prefix("PASSED.") {
                    // PASSED.store.X → store.X in reactive_vars
                    if self.reactive_vars.contains_key(without_passed) {
                        deps.push(without_passed.to_string());
                    }
                }
            }
            Expression::Pipe { from, to } => {
                self.collect_reactive_deps(from, deps);
                self.collect_reactive_deps(to, deps);
            }
            Expression::When { arms } => {
                for arm in arms {
                    self.collect_reactive_deps(&arm.body, deps);
                }
            }
            Expression::FunctionCall { arguments, .. } => {
                for arg in arguments {
                    if let Some(ref value) = arg.node.value {
                        self.collect_reactive_deps(value, deps);
                    }
                }
            }
            Expression::ArithmeticOperator(op) => {
                match op {
                    ArithmeticOperator::Add { operand_a, operand_b }
                    | ArithmeticOperator::Subtract { operand_a, operand_b }
                    | ArithmeticOperator::Multiply { operand_a, operand_b }
                    | ArithmeticOperator::Divide { operand_a, operand_b } => {
                        self.collect_reactive_deps(operand_a, deps);
                        self.collect_reactive_deps(operand_b, deps);
                    }
                    ArithmeticOperator::Negate { operand } => {
                        self.collect_reactive_deps(operand, deps);
                    }
                }
            }
            _ => {}
        }
    }

    /// Check if a collection has a guaranteed initial value (not empty until event).
    ///
    /// Returns true for HOLDs (have initial value), Literals, and derivations thereof.
    /// Returns false for event-only Inputs (LINK events) and their derivatives.
    /// Used to filter document dependencies — event-only vars block the Join chain.
    fn has_initial_value(&self, var_id: &VarId) -> bool {
        match self.collections.get(var_id) {
            Some(CollectionSpec::Literal(_)) => true,
            Some(CollectionSpec::LiteralList(_)) => true,
            Some(CollectionSpec::HoldState { .. }) => true,
            Some(CollectionSpec::KeyedHoldState { .. }) => true,
            Some(CollectionSpec::Input(input_id)) => {
                // Router inputs have initial values (current route), LINK inputs don't
                self.inputs.iter().any(|spec| spec.id == *input_id && spec.kind == InputKind::Router)
            }
            Some(CollectionSpec::Map { source, .. }) => self.has_initial_value(source),
            Some(CollectionSpec::Then { source, .. }) => self.has_initial_value(source),
            Some(CollectionSpec::FlatMap { .. }) => {
                // FlatMap (when_match) may filter items — can't guarantee initial value
                false
            }
            Some(CollectionSpec::Join { left, right, .. }) => {
                self.has_initial_value(left) && self.has_initial_value(right)
            }
            Some(CollectionSpec::HoldLatest(sources)) => {
                sources.iter().any(|s| self.has_initial_value(s))
            }
            Some(CollectionSpec::Concat(sources)) => {
                sources.iter().any(|s| self.has_initial_value(s))
            }
            Some(CollectionSpec::Skip { source, .. }) => self.has_initial_value(source),
            Some(CollectionSpec::SideEffect { source, .. }) => self.has_initial_value(source),
            Some(CollectionSpec::ListCount(source)) => self.has_initial_value(source),
            Some(CollectionSpec::ListRetain { source, .. }) => self.has_initial_value(source),
            Some(CollectionSpec::ListRetainReactive { list, filter_state, .. }) => {
                self.has_initial_value(list) && self.has_initial_value(filter_state)
            }
            Some(CollectionSpec::ListMap { source, .. }) => self.has_initial_value(source),
            Some(CollectionSpec::ListMapWithKey { source, .. }) => self.has_initial_value(source),
            Some(CollectionSpec::ListAppend { list, .. }) => self.has_initial_value(list),
            Some(CollectionSpec::ListRemove { list, .. }) => self.has_initial_value(list),
            Some(CollectionSpec::MapToKeyed { .. }) => false, // Event stream, no initial value
            Some(CollectionSpec::AppendNewKeyed { .. }) => false, // Event stream, no initial value
            Some(CollectionSpec::AssembleList(source)) => self.has_initial_value(source),
            Some(CollectionSpec::KeyedConcat(sources)) => {
                sources.iter().any(|s| self.has_initial_value(s))
            }
            None => false,
        }
    }

    /// Compile `LIST {} |> List/append(item: X) |> List/clear(on: Y)` as a HoldState.
    ///
    /// The items list is maintained as a scalar Value::List in a HoldState.
    /// Append events come from the text_to_add reactive var (via Then).
    /// Clear events come from a button press input (via Then).
    fn compile_list_chain(
        &mut self,
        name: &str,
        from: &Spanned<Expression>,
        clear_args: &[Spanned<Argument>],
    ) -> Result<VarId, String> {
        // Extract the clear trigger from arguments: `on: event_source`
        let clear_source_expr = clear_args.iter()
            .find(|a| a.node.name.as_str() == "on")
            .and_then(|a| a.node.value.as_ref())
            .ok_or_else(|| "List/clear missing 'on' argument".to_string())?;
        let (clear_input_var, _) = self.compile_event_source(clear_source_expr)?;

        // Unwrap the from to find List/append
        let append_source = match &from.node {
            Expression::Pipe { from: inner_from, to: inner_to } => {
                match &inner_to.node {
                    Expression::FunctionCall { path, arguments }
                        if {
                            let p: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                            p.as_slice() == ["List", "append"]
                        } =>
                    {
                        // Extract the `item:` argument (the reactive var to append)
                        let item_arg = arguments.iter()
                            .find(|a| a.node.name.as_str() == "item")
                            .and_then(|a| a.node.value.as_ref())
                            .ok_or_else(|| "List/append missing 'item' argument".to_string())?;

                        // Resolve the item source (should be a reactive var like text_to_add)
                        self.resolve_reactive_source(item_arg)?
                    }
                    _ => return Err("Expected List/append in List/clear chain".to_string()),
                }
            }
            _ => return Err("Expected pipe chain before List/clear".to_string()),
        };

        // Create Then wrappers for append and clear events (fire-and-forget)
        let append_then_var = self.fresh_var("append_event");
        self.collections.insert(
            append_then_var.clone(),
            CollectionSpec::Then {
                source: append_source,
                body: Arc::new(|v: &Value| {
                    Value::object([("__t", Value::text("append")), ("v", v.clone())])
                }),
            },
        );

        let clear_then_var = self.fresh_var("clear_event");
        self.collections.insert(
            clear_then_var.clone(),
            CollectionSpec::Then {
                source: clear_input_var,
                body: Arc::new(|_v: &Value| {
                    Value::object([("__t", Value::text("clear"))])
                }),
            },
        );

        // Concat append + clear events
        let events_var = self.fresh_var("list_events");
        self.collections.insert(
            events_var.clone(),
            CollectionSpec::Concat(vec![append_then_var, clear_then_var]),
        );

        // HoldState: maintains the items list
        let initial_list = Value::empty_list();
        let initial_var = self.fresh_var("list_init");
        self.collections.insert(initial_var.clone(), CollectionSpec::Literal(initial_list.clone()));

        // Check for persisted items
        let effective_initial = self.persisted_holds
            .get(name)
            .cloned()
            .unwrap_or_else(|| initial_list.clone());
        // Override initial collection if persisted
        if self.persisted_holds.contains_key(name) {
            self.collections.insert(initial_var.clone(), CollectionSpec::Literal(effective_initial.clone()));
        }

        let list_var = VarId::new(name);
        self.collections.insert(
            list_var.clone(),
            CollectionSpec::HoldState {
                initial: initial_var,
                events: events_var,
                initial_value: effective_initial,
                transform: Arc::new(|state: &Value, event: &Value| {
                    let event_type = event.get_field("__t")
                        .and_then(|v| v.as_text())
                        .unwrap_or("")
                        .to_string();
                    match event_type.as_str() {
                        "append" => {
                            let item = event.get_field("v").cloned().unwrap_or(Value::Unit);
                            let count = state.list_count();
                            state.list_append(item, count)
                        }
                        "clear" => Value::empty_list(),
                        _ => state.clone(),
                    }
                }),
            },
        );

        self.reactive_vars.insert(name.to_string(), list_var.clone());

        // Emit persistence side effect if storage_key is set
        if let Some(key) = self.storage_key.clone() {
            let persist_var = self.fresh_var(&format!("{}_persist", name));
            self.collections.insert(
                persist_var,
                CollectionSpec::SideEffect {
                    source: list_var.clone(),
                    effect: SideEffectKind::PersistHold {
                        key,
                        hold_name: name.to_string(),
                    },
                },
            );
        }

        Ok(list_var)
    }

    /// Compile `LIST {} |> List/append(item: X)` without clear.
    fn compile_list_append_only(
        &mut self,
        name: &str,
        from: &Spanned<Expression>,
        arguments: &[Spanned<Argument>],
    ) -> Result<VarId, String> {
        // Extract item source
        let item_arg = arguments.iter()
            .find(|a| a.node.name.as_str() == "item")
            .and_then(|a| a.node.value.as_ref())
            .ok_or_else(|| "List/append missing 'item' argument".to_string())?;
        let item_source = self.resolve_reactive_source(item_arg)?;

        // Create Then wrapper for append events
        let append_then_var = self.fresh_var("append_event");
        self.collections.insert(
            append_then_var.clone(),
            CollectionSpec::Then {
                source: item_source,
                body: Arc::new(|v: &Value| {
                    Value::object([("__t", Value::text("append")), ("v", v.clone())])
                }),
            },
        );

        // HoldState: maintains the items list
        let initial_list = Value::empty_list();
        let initial_var = self.fresh_var("list_init");
        self.collections.insert(initial_var.clone(), CollectionSpec::Literal(initial_list.clone()));

        let list_var = VarId::new(name);
        self.collections.insert(
            list_var.clone(),
            CollectionSpec::HoldState {
                initial: initial_var,
                events: append_then_var,
                initial_value: initial_list,
                transform: Arc::new(|state: &Value, event: &Value| {
                    let item = event.get_field("v").cloned().unwrap_or(Value::Unit);
                    let count = state.list_count();
                    state.list_append(item, count)
                }),
            },
        );

        self.reactive_vars.insert(name.to_string(), list_var.clone());
        Ok(list_var)
    }

}

/// Handle a wildcard event for per-item list operations.
///
/// The `path` is the event path (e.g., "store.todos.0001.todo_elements.remove_todo_button").
/// The `state` is the full list (a Value::Tagged with tag "List").
/// Extract the list item key from a wildcard event path.
///
/// Paths have the form: `store.todos.0001.todo_elements.remove_todo_button`
/// This extracts the 4-digit key (e.g., "0001") that identifies the list item.
fn extract_list_item_key_from_path(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split('.').collect();
    // Look for a 4-digit numeric key in the path
    for part in &parts {
        if part.len() == 4 && part.chars().all(|c| c.is_ascii_digit()) {
            return Some(part.to_string());
        }
    }
    None
}

fn set_nested_field(obj: &Value, path: &str, value: Value) -> Value {
    let parts: Vec<&str> = path.splitn(2, '.').collect();
    if parts.len() == 1 {
        obj.update_field(parts[0], value)
    } else {
        let existing = obj.get_field(parts[0]).cloned().unwrap_or_else(|| Value::Object(Arc::new(BTreeMap::new())));
        let updated = set_nested_field(&existing, parts[1], value);
        obj.update_field(parts[0], updated)
    }
}

/// Inject __link_path__ fields into a list item's todo_elements.
/// Inject __link_path__ fields into a list item with a specific key.
fn inject_item_link_paths_with_key(item: &Value, list_path: &str, key: &str) -> Value {
    let item_path = format!("{}.{}", list_path, key);
    let mut result = item.clone();

    // Inject hover path for per-item hover (element.hovered).
    // Preserve existing __hovered state (set by wildcard HoverChange events).
    if item.get_field("__hovered").is_none() {
        result = result.update_field("__hovered", Value::tag("False"));
    }
    result = result.update_field("__hover_path__", Value::text(format!("{}.hovered", item_path).as_str()));

    // If the item has todo_elements, inject link paths
    if let Some(todo_elements) = item.get_field("todo_elements") {
        let mut new_elements = todo_elements.clone();
        if let Value::Object(fields) = &todo_elements {
            for (el_name, _) in fields.iter() {
                let link_path = format!("{}.todo_elements.{}", item_path, el_name);
                new_elements = new_elements.update_field(
                    el_name.as_ref(),
                    Value::object([("__link_path__", Value::text(link_path.as_str()))]),
                );
            }
        }
        result = result.update_field("todo_elements", new_elements);
    }

    result
}

// ---------------------------------------------------------------------------
// Document template — captures static structure for reactive document building
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum DocTemplate {
    /// Static value (no reactive dependencies)
    Static(Value),
    /// Reference to the reactive variable
    ReactiveRef,
    /// Tagged value with possibly-reactive fields
    Tagged {
        tag: String,
        fields: Vec<(String, DocTemplate)>,
    },
    /// Text with reactive interpolation
    TextInterpolation(Vec<TextPartTemplate>),
}

#[derive(Clone)]
enum TextPartTemplate {
    Literal(String),
    ReactiveRef,
}

impl DocTemplate {
    fn instantiate(&self, reactive_value: &Value) -> Value {
        match self {
            DocTemplate::Static(v) => v.clone(),
            DocTemplate::ReactiveRef => reactive_value.clone(),
            DocTemplate::Tagged { tag, fields } => {
                let field_map: BTreeMap<Arc<str>, Value> = fields
                    .iter()
                    .map(|(name, tmpl)| {
                        (Arc::from(name.as_str()), tmpl.instantiate(reactive_value))
                    })
                    .collect();
                Value::Tagged {
                    tag: Arc::from(tag.as_str()),
                    fields: Arc::new(field_map),
                }
            }
            DocTemplate::TextInterpolation(parts) => {
                let mut result = String::new();
                for part in parts {
                    match part {
                        TextPartTemplate::Literal(s) => result.push_str(s),
                        TextPartTemplate::ReactiveRef => {
                            result.push_str(&reactive_value.to_display_string());
                        }
                    }
                }
                Value::text(result)
            }
        }
    }
}

impl Compiler {
    fn build_doc_template(
        &self,
        reactive_var_name: &str,
        expr: &Spanned<Expression>,
    ) -> Result<DocTemplate, String> {
        self.build_doc_template_inner(reactive_var_name, expr, None)
    }

    fn build_doc_template_keyed(
        &self,
        reactive_var_name: &str,
        expr: &Spanned<Expression>,
        keyed_list_name: Option<&str>,
    ) -> Result<DocTemplate, String> {
        self.build_doc_template_inner(reactive_var_name, expr, keyed_list_name)
    }

    fn build_doc_template_inner(
        &self,
        reactive_var_name: &str,
        expr: &Spanned<Expression>,
        keyed_list_name: Option<&str>,
    ) -> Result<DocTemplate, String> {
        match &expr.node {
            Expression::Alias(Alias::WithoutPassed { parts, .. }) => {
                let name = parts[0].as_str();
                if name == reactive_var_name && parts.len() == 1 {
                    Ok(DocTemplate::ReactiveRef)
                } else if parts.len() == 1 {
                    if let Some(var_expr) = self.get_var_expr(name).cloned() {
                        self.build_doc_template_inner(reactive_var_name, &var_expr, keyed_list_name)
                    } else {
                        let val = self.eval_static(expr)?;
                        Ok(DocTemplate::Static(val))
                    }
                } else {
                    let val = self.eval_static(expr)?;
                    Ok(DocTemplate::Static(val))
                }
            }

            Expression::FunctionCall { path, arguments } => {
                let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                match path_strs.as_slice() {
                    ["Document", "new"] => {
                        let mut field_templates = Vec::new();
                        for arg in arguments {
                            let name = arg.node.name.as_str().to_string();
                            if let Some(ref val_expr) = arg.node.value {
                                let tmpl = self.build_doc_template_inner(
                                    reactive_var_name,
                                    val_expr,
                                    keyed_list_name,
                                )?;
                                field_templates.push((name, tmpl));
                            }
                        }
                        Ok(DocTemplate::Tagged {
                            tag: "DocumentNew".to_string(),
                            fields: field_templates,
                        })
                    }
                    ["Element", elem_type] => {
                        let tag = format!("Element{}", capitalize(elem_type));
                        let mut field_templates = Vec::new();
                        for arg in arguments {
                            let name = arg.node.name.as_str().to_string();
                            if let Some(ref val_expr) = arg.node.value {
                                // For "items:" on a Stripe, check if this expression
                                // is the keyed display pipeline (PASSED.store.<name> |> ...)
                                // and emit an empty list so the bridge uses keyed diffs.
                                let tmpl = if name == "items" && *elem_type == "stripe" {
                                    if let Some(kln) = keyed_list_name {
                                        if self.expr_references_passed_store(val_expr, kln) {
                                            DocTemplate::Static(Value::empty_list())
                                        } else {
                                            self.build_doc_template_inner(
                                                reactive_var_name,
                                                val_expr,
                                                keyed_list_name,
                                            )?
                                        }
                                    } else {
                                        self.build_doc_template_inner(
                                            reactive_var_name,
                                            val_expr,
                                            keyed_list_name,
                                        )?
                                    }
                                } else {
                                    self.build_doc_template_inner(
                                        reactive_var_name,
                                        val_expr,
                                        keyed_list_name,
                                    )?
                                };
                                // Check for LINK binding and add press_link
                                if name == "element" {
                                    if self.expr_contains_link(val_expr) {
                                        // Find the link path for this element
                                        let link_path = self.find_link_path_for_element(
                                            reactive_var_name,
                                            val_expr,
                                        );
                                        if let Some(path) = link_path {
                                            field_templates.push((
                                                "press_link".to_string(),
                                                DocTemplate::Static(Value::text(path)),
                                            ));
                                        }
                                    }
                                }
                                field_templates.push((name, tmpl));
                            }
                        }
                        Ok(DocTemplate::Tagged {
                            tag,
                            fields: field_templates,
                        })
                    }
                    _ => {
                        let val = self.eval_static(expr)?;
                        Ok(DocTemplate::Static(val))
                    }
                }
            }

            Expression::Pipe { from, to } => {
                match &to.node {
                    Expression::FunctionCall { .. } => {
                        self.build_doc_template_inner(reactive_var_name, to, keyed_list_name)
                            .or_else(|_| {
                                let val = self.eval_static(expr).unwrap_or(Value::Unit);
                                Ok(DocTemplate::Static(val))
                            })
                    }
                    _ => {
                        let val = self.eval_static(expr).unwrap_or(Value::Unit);
                        Ok(DocTemplate::Static(val))
                    }
                }
            }

            Expression::List { items } => {
                let mut item_templates = Vec::new();
                for (i, item) in items.iter().enumerate() {
                    let tmpl = self.build_doc_template_inner(reactive_var_name, item, keyed_list_name)?;
                    item_templates.push((format!("{:04}", i), tmpl));
                }
                Ok(DocTemplate::Tagged {
                    tag: "List".to_string(),
                    fields: item_templates,
                })
            }

            Expression::TextLiteral { parts } => {
                let has_reactive = parts.iter().any(|p| match p {
                    TextPart::Interpolation { var, .. } => var.as_str() == reactive_var_name,
                    _ => false,
                });

                if has_reactive {
                    let parts_clone: Vec<_> = parts
                        .iter()
                        .map(|p| match p {
                            TextPart::Text(s) => TextPartTemplate::Literal(s.as_str().to_string()),
                            TextPart::Interpolation { var, .. } => {
                                if var.as_str() == reactive_var_name {
                                    TextPartTemplate::ReactiveRef
                                } else {
                                    let val = self
                                        .resolve_alias_static(var.as_str(), &IndexMap::new())
                                        .unwrap_or(Value::Unit);
                                    TextPartTemplate::Literal(val.to_display_string())
                                }
                            }
                        })
                        .collect();
                    Ok(DocTemplate::TextInterpolation(parts_clone))
                } else {
                    let val = self.eval_static(expr)?;
                    Ok(DocTemplate::Static(val))
                }
            }

            Expression::Link => {
                Ok(DocTemplate::Static(Value::tag("LINK")))
            }

            Expression::Object(obj) => {
                let mut field_templates = Vec::new();
                for var in &obj.variables {
                    let name = var.node.name.as_str().to_string();
                    let tmpl = self.build_doc_template_inner(
                        reactive_var_name,
                        &var.node.value,
                        keyed_list_name,
                    )?;
                    field_templates.push((name, tmpl));
                }
                Ok(DocTemplate::Tagged {
                    tag: "Object".to_string(),
                    fields: field_templates,
                })
            }

            Expression::Literal(_)
            | Expression::TaggedObject { .. } => {
                let val = self.eval_static(expr)?;
                Ok(DocTemplate::Static(val))
            }

            _ => {
                match self.eval_static(expr) {
                    Ok(val) => Ok(DocTemplate::Static(val)),
                    Err(e) => Err(format!("Cannot build doc template: {}", e)),
                }
            }
        }
    }

    fn find_link_path_for_element(
        &self,
        _reactive_var_name: &str,
        _element_expr: &Spanned<Expression>,
    ) -> Option<String> {
        // Walk through top-level variables to find which one contains this LINK
        for (name, expr) in &self.variables {
            if self.expr_contains_link(expr) {
                if let Expression::FunctionCall { .. } = &expr.node {
                    return Some(format!("{}.event.press", name));
                }
            }
        }
        None
    }

    /// Check if an expression (or any sub-expression in a Pipe) references
    /// `PASSED.store.<name>` for the given short name.
    fn expr_references_passed_store(&self, expr: &Spanned<Expression>, short_name: &str) -> bool {
        match &expr.node {
            Expression::Alias(Alias::WithPassed { extra_parts }) => {
                let parts: Vec<&str> = extra_parts.iter().map(|s| s.as_str()).collect();
                parts == ["store", short_name]
            }
            Expression::Pipe { from, to } => {
                self.expr_references_passed_store(from, short_name)
                    || self.expr_references_passed_store(to, short_name)
            }
            _ => false,
        }
    }

    /// Find the element tag of the Stripe that displays keyed list items.
    /// Scans function bodies for `Element/stripe(element: [tag: X], items: PASSED.store.<name> |> ...)`
    /// and returns the tag X (e.g., "Ul" for todo_mvc's todos_element).
    fn find_keyed_stripe_element_tag(&self, keyed_list_short_name: &str) -> Option<String> {
        for (_, _, body) in &self.functions {
            if let Some(tag) = self.find_keyed_stripe_tag_in_expr(body, keyed_list_short_name) {
                return Some(tag);
            }
        }
        None
    }

    fn find_keyed_stripe_tag_in_expr(
        &self,
        expr: &Spanned<Expression>,
        keyed_list_short_name: &str,
    ) -> Option<String> {
        match &expr.node {
            Expression::FunctionCall { path, arguments } => {
                let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                if let ["Element", "stripe"] = path_strs.as_slice() {
                    // Check if items: references the keyed list
                    let has_keyed_items = arguments.iter().any(|arg| {
                        arg.node.name.as_str() == "items"
                            && arg.node.value.as_ref().map_or(false, |v| {
                                self.expr_references_passed_store(v, keyed_list_short_name)
                            })
                    });
                    if has_keyed_items {
                        // Extract tag from element: [tag: X]
                        for arg in arguments {
                            if arg.node.name.as_str() == "element" {
                                if let Some(ref val_expr) = arg.node.value {
                                    if let Ok(val) = self.eval_static(val_expr) {
                                        if let Some(tag) = val.get_field("tag").and_then(|t| t.as_tag()) {
                                            return Some(tag.to_string());
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                // Recurse into arguments
                for arg in arguments {
                    if let Some(ref val_expr) = arg.node.value {
                        if let Some(tag) = self.find_keyed_stripe_tag_in_expr(val_expr, keyed_list_short_name) {
                            return Some(tag);
                        }
                    }
                }
                None
            }
            Expression::Pipe { from, to } => {
                self.find_keyed_stripe_tag_in_expr(from, keyed_list_short_name)
                    .or_else(|| self.find_keyed_stripe_tag_in_expr(to, keyed_list_short_name))
            }
            Expression::Block { output, .. } => {
                self.find_keyed_stripe_tag_in_expr(output, keyed_list_short_name)
            }
            _ => None,
        }
    }

    fn expr_contains_link(&self, expr: &Spanned<Expression>) -> bool {
        match &expr.node {
            Expression::Link => true,
            Expression::Object(obj) => obj
                .variables
                .iter()
                .any(|v| self.expr_contains_link(&v.node.value)),
            Expression::List { items } => items.iter().any(|i| self.expr_contains_link(i)),
            Expression::FunctionCall { arguments, .. } => {
                arguments.iter().any(|a| {
                    a.node.value.as_ref()
                        .map(|v| self.expr_contains_link(v))
                        .unwrap_or(false)
                })
            }
            _ => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Internal types
// ---------------------------------------------------------------------------

enum HoldTransform {
    Increment(f64),
    Custom,
}

fn is_alias_named(expr: &Expression, name: &str) -> bool {
    match expr {
        Expression::Alias(Alias::WithoutPassed { parts, .. }) => {
            parts.len() == 1 && parts[0].as_str() == name
        }
        _ => false,
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().to_string() + c.as_str(),
    }
}
