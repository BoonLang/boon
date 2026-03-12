//! Boon source → compiled program.
//!
//! Parses Boon source code and compiles it into a `CompiledProgram`:
//! - Static programs (no LINK/HOLD/Timer) → evaluated to a Value
//! - Reactive programs → DataflowGraph with CollectionSpec entries
//!
//! Uses the existing Boon parser. The compiler walks the static AST
//! and evaluates/compiles expressions.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use boon_scene::RenderSurface;
use indexmap::IndexMap;

use crate::parser::{
    SourceCode, lexer, parser, reset_expression_depth, resolve_references, span_at,
    static_expression::{
        self, Alias, Argument, ArithmeticOperator, Comparator, Expression, Literal, Spanned,
        TextPart,
    },
};

use super::types::{
    BroadcastHandlerFn, CollectionSpec, DEP_FIELD_PREFIX, DataflowGraph, HOVER_PATH_FIELD,
    HOVERED_FIELD, HoldTransformFn, InputId, InputKind, InputSpec, KEYED_LIST_NAME_FIELD,
    KeyedListOutput, LINK_PATH_FIELD, LIST_TAG, ListKey, PASSED_VAR, ROUTER_INPUT, SideEffectKind,
    VarId,
};
use super::value::Value;

/// Result of compiling a Boon program.
pub enum CompiledProgram {
    /// Purely static program — no reactive computation.
    Static {
        document_value: Value,
        render_surface: RenderSurface,
    },

    /// Reactive program compiled to a DD dataflow graph.
    /// All reactive computation flows through DD collections and operators.
    Dataflow { graph: DataflowGraph },
}

/// Operations collected from a `LIST {} |> List/append(...) |> List/remove(...)` chain.
enum ListChainOp<'a> {
    /// `List/append(item: source_expr)`
    Append(&'a [Spanned<Argument>]),
    /// `List/remove(item, on: condition_expr)`
    Remove(&'a [Spanned<Argument>]),
    /// `List/remove_last(on: event_source)`
    RemoveLast(&'a [Spanned<Argument>]),
    /// `List/clear(on: event_source)`
    Clear(&'a [Spanned<Argument>]),
    /// `List/retain(item, if: predicate_expr)`
    Retain(&'a [Spanned<Argument>]),
}

/// External function definition for multi-file support.
/// (qualified_name, params, body, module_name)
pub type ExternalFunction = (String, Vec<String>, Spanned<Expression>, Option<String>);

/// Compile Boon source code into a program.
pub fn compile(
    source_code: &str,
    storage_key: Option<&str>,
    persisted_holds: &std::collections::HashMap<String, Value>,
    external_functions: Option<&[ExternalFunction]>,
) -> Result<CompiledProgram, String> {
    let is_cells = source_code.contains("-- 7GUIs Task 7: Cells (Spreadsheet)");
    if is_cells {
        std::println!("[cells-dd] compile: start");
    }
    let ast = parse_source(source_code)?;
    if is_cells {
        std::println!("[cells-dd] compile: parsed source");
    }

    // Build top-level scope
    let mut compiler = Compiler::new();
    compiler.register_top_level(&ast);
    if is_cells {
        std::println!("[cells-dd] compile: registered top level");
    }

    // Register external functions from other module files
    if let Some(ext_fns) = external_functions {
        compiler.register_external_functions(ext_fns);
        if is_cells {
            std::println!("[cells-dd] compile: registered external functions");
        }
    }

    // Find the output variable: try "scene" first, then "document".
    // When "scene" is used, the value is a Scene/new(...) call — the DD renderer
    // will extract the root element from it (physical CSS properties are ignored for now).
    let (render_surface, doc_expr) = if let Some(expr) = compiler.get_var_expr("scene") {
        (RenderSurface::Scene, expr.clone())
    } else if let Some(expr) = compiler.get_var_expr("document") {
        (RenderSurface::Document, expr.clone())
    } else {
        return Err("No 'scene' or 'document' variable found".to_string());
    };

    // Check if program is reactive
    let has_reactive = compiler.has_reactive_constructs();
    if is_cells {
        std::println!("[cells-dd] compile: has_reactive_constructs = {has_reactive}");
    }
    if has_reactive {
        // Compile to DataflowGraph
        if is_cells {
            std::println!("[cells-dd] compile: before compile_to_graph");
        }
        match compiler.compile_to_graph(&doc_expr, render_surface, storage_key, persisted_holds) {
            Ok(graph) => Ok(CompiledProgram::Dataflow { graph }),
            Err(e) => Err(format!("Reactive compilation failed: {}", e)),
        }
    } else {
        // Try static evaluation
        match compiler.eval_static(&doc_expr) {
            Ok(value) => Ok(CompiledProgram::Static {
                document_value: value,
                render_surface,
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

    let is_cells = source_code.contains("-- 7GUIs Task 7: Cells (Spreadsheet)");
    if is_cells {
        std::println!("[cells-dd] parse_source: start");
    }
    let source_code_arc = SourceCode::new(source_code.to_string());
    let source_for_parsing = source_code_arc.clone();
    let source_ref = source_for_parsing.as_str();

    let (tokens, lex_errors) = lexer().parse(source_ref).into_output_errors();
    if is_cells {
        std::println!("[cells-dd] parse_source: lexed");
    }
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
    if is_cells {
        std::println!("[cells-dd] parse_source: parsed");
    }

    let ast = resolve_references(ast).map_err(|e| format!("Reference errors: {:?}", e))?;
    if is_cells {
        std::println!("[cells-dd] parse_source: resolved references");
    }

    // Convert to static expressions
    let static_ast = static_expression::convert_expressions(source_code_arc, ast);
    if is_cells {
        std::println!("[cells-dd] parse_source: converted to static");
    }
    Ok(static_ast)
}

// ---------------------------------------------------------------------------
// Compiler
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Compiler {
    /// Top-level variable definitions: name → expression
    variables: Arc<Vec<(String, Spanned<Expression>)>>,
    /// Function definitions: name → (params, body)
    functions: Arc<Vec<(String, Vec<String>, Spanned<Expression>)>>,
    /// Module name for each qualified function (e.g., "Theme/get" → "Theme").
    /// Used for intra-module resolution: when inside Theme/get, an unqualified
    /// call to material() resolves to Theme/material.
    function_modules: HashMap<String, String>,
    /// Current module context during function body evaluation.
    current_module: Option<String>,
    /// Cache for user-defined function external-input classification.
    external_input_function_cache: RefCell<HashMap<String, bool>>,
}

impl Compiler {
    fn new() -> Self {
        Self {
            variables: Arc::new(Vec::new()),
            functions: Arc::new(Vec::new()),
            function_modules: HashMap::new(),
            current_module: None,
            external_input_function_cache: RefCell::new(HashMap::new()),
        }
    }

    /// Register external functions from parsed module files.
    fn register_external_functions(&mut self, ext_fns: &[ExternalFunction]) {
        for (qualified_name, params, body, module_name) in ext_fns {
            Arc::make_mut(&mut self.functions).push((
                qualified_name.clone(),
                params.clone(),
                body.clone(),
            ));
            if let Some(module) = module_name {
                self.function_modules
                    .insert(qualified_name.clone(), module.clone());
            }
        }
    }

    /// Look up a function by name, with intra-module fallback.
    /// First tries an exact match, then tries module-qualified name
    /// if we're currently inside a module function.
    fn find_function(&self, name: &str) -> Option<&(String, Vec<String>, Spanned<Expression>)> {
        // Try exact match
        if let Some(f) = self.functions.iter().find(|(n, _, _)| n == name) {
            return Some(f);
        }
        // Intra-module resolution: try current_module/name
        if let Some(module) = &self.current_module {
            let qualified = format!("{}/{}", module, name);
            if let Some(f) = self.functions.iter().find(|(n, _, _)| n == &qualified) {
                return Some(f);
            }
        }
        None
    }

    /// Create a scoped clone with the current_module set for the given function.
    fn with_module_context(&self, qualified_fn_name: &str) -> Compiler {
        let mut c = self.clone();
        c.current_module = self.function_modules.get(qualified_fn_name).cloned();
        c
    }

    /// Check if a function path is a known built-in piped function.
    fn is_builtin_piped_fn(&self, path: &[String]) -> bool {
        let strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
        matches!(
            strs.as_slice(),
            ["Bool", "not"]
                | ["Bool", "or"]
                | ["Bool", "and"]
                | ["Text", "trim"]
                | ["Text", "is_not_empty"]
                | ["Text", "is_empty"]
                | ["Text", "to_number"]
                | ["Text", "starts_with"]
                | ["Text", "length"]
                | ["Text", "char_at"]
                | ["Text", "char_code"]
                | ["Text", "from_char_code"]
                | ["Text", "find"]
                | ["Text", "find_closing"]
                | ["Text", "substring"]
                | ["Text", "to_uppercase"]
                | ["Math", "round"]
                | ["Math", "min"]
                | ["Math", "max"]
                | ["Math", "modulo"]
                | ["List", "count"]
                | ["List", "is_empty"]
                | ["List", "sum"]
                | ["List", "product"]
                | ["List", "last"]
                | ["List", "get"]
                | ["Log", "info"]
        )
    }

    /// Evaluate a built-in piped function with a runtime Value input.
    /// Mirrors the static evaluator's piped dispatch but takes a Value directly.
    fn eval_builtin_piped(
        &self,
        input: &Value,
        path: &[String],
        arguments: &[(String, Option<Spanned<Expression>>)],
    ) -> Result<Value, String> {
        let strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
        match strs.as_slice() {
            ["Log", "info"] => Ok(input.clone()),
            ["Text", "trim"] => {
                let s = input.as_text().unwrap_or("");
                Ok(Value::text(s.trim()))
            }
            ["Text", "is_not_empty"] => {
                let s = input.as_text().unwrap_or("");
                Ok(if !s.is_empty() {
                    Value::tag("True")
                } else {
                    Value::tag("False")
                })
            }
            ["Text", "is_empty"] => {
                let s = input.as_text().unwrap_or("");
                Ok(if s.is_empty() {
                    Value::tag("True")
                } else {
                    Value::tag("False")
                })
            }
            ["Bool", "not"] => {
                let b = input.as_bool().unwrap_or(false);
                Ok(if b {
                    Value::tag("False")
                } else {
                    Value::tag("True")
                })
            }
            ["Bool", "or"] => {
                let a = input.as_bool().unwrap_or(false);
                let b = arguments
                    .iter()
                    .find(|(name, _)| name == "that")
                    .and_then(|(_, val_expr)| val_expr.as_ref())
                    .and_then(|v| self.eval_static(v).ok())
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                Ok(if a || b {
                    Value::tag("True")
                } else {
                    Value::tag("False")
                })
            }
            ["Bool", "and"] => {
                let a = input.as_bool().unwrap_or(false);
                let b = arguments
                    .iter()
                    .find(|(name, _)| name == "that")
                    .and_then(|(_, val_expr)| val_expr.as_ref())
                    .and_then(|v| self.eval_static(v).ok())
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                Ok(if a && b {
                    Value::tag("True")
                } else {
                    Value::tag("False")
                })
            }
            ["List", "count"] => Ok(Value::number(input.list_count() as f64)),
            ["List", "is_empty"] => Ok(if input.list_is_empty() {
                Value::tag("True")
            } else {
                Value::tag("False")
            }),
            ["Text", "to_number"] => {
                let s = input.as_text().unwrap_or("");
                match s.trim().parse::<f64>() {
                    Ok(n) => Ok(Value::number(n)),
                    Err(_) => Ok(Value::tag("NaN")),
                }
            }
            ["Text", "starts_with"] => {
                let s = input.as_text().unwrap_or("");
                let prefix = arguments
                    .iter()
                    .find(|(name, _)| name == "prefix")
                    .and_then(|(_, val_expr)| val_expr.as_ref())
                    .and_then(|v| self.eval_static(v).ok())
                    .and_then(|v| v.as_text().map(|s| s.to_string()))
                    .unwrap_or_default();
                Ok(if s.starts_with(&prefix) {
                    Value::tag("True")
                } else {
                    Value::tag("False")
                })
            }
            ["Text", "length"] => {
                let s = input.as_text().unwrap_or("");
                Ok(Value::number(s.len() as f64))
            }
            ["Text", "char_at"] => {
                let s = input.as_text().unwrap_or("");
                let index = arguments
                    .iter()
                    .find(|(name, _)| name == "index")
                    .and_then(|(_, val_expr)| val_expr.as_ref())
                    .and_then(|v| self.eval_static(v).ok())
                    .and_then(|v| v.as_number())
                    .unwrap_or(0.0) as usize;
                match s.chars().nth(index) {
                    Some(c) => Ok(Value::text(c.to_string())),
                    None => Ok(Value::text("")),
                }
            }
            ["Text", "char_code"] => {
                let s = input.as_text().unwrap_or("");
                match s.chars().next() {
                    Some(c) => Ok(Value::number(c as u32 as f64)),
                    None => Ok(Value::number(0.0)),
                }
            }
            ["Text", "from_char_code"] => {
                let n = input.as_number().unwrap_or(0.0) as u32;
                match char::from_u32(n) {
                    Some(c) => Ok(Value::text(c.to_string())),
                    None => Ok(Value::text("")),
                }
            }
            ["Text", "find"] => {
                let s = input.as_text().unwrap_or("");
                let search = arguments
                    .iter()
                    .find(|(name, _)| name == "search")
                    .and_then(|(_, val_expr)| val_expr.as_ref())
                    .and_then(|v| self.eval_static(v).ok())
                    .and_then(|v| v.as_text().map(|s| s.to_string()))
                    .unwrap_or_default();
                match s.find(&search) {
                    Some(pos) => Ok(Value::number(pos as f64)),
                    None => Ok(Value::number(-1.0)),
                }
            }
            ["Text", "find_closing"] => {
                let s = input.as_text().unwrap_or("");
                let open = arguments
                    .iter()
                    .find(|(name, _)| name == "open")
                    .and_then(|(_, val_expr)| val_expr.as_ref())
                    .and_then(|v| self.eval_static(v).ok())
                    .and_then(|v| v.as_text().map(|s| s.to_string()))
                    .unwrap_or_else(|| "(".to_string());
                let close = arguments
                    .iter()
                    .find(|(name, _)| name == "close")
                    .and_then(|(_, val_expr)| val_expr.as_ref())
                    .and_then(|v| self.eval_static(v).ok())
                    .and_then(|v| v.as_text().map(|s| s.to_string()))
                    .unwrap_or_else(|| ")".to_string());
                let open_char = open.chars().next().unwrap_or('(');
                let close_char = close.chars().next().unwrap_or(')');
                let mut depth = 0i32;
                let mut result = -1i32;
                for (i, c) in s.chars().enumerate() {
                    if c == open_char {
                        depth += 1;
                    } else if c == close_char {
                        depth -= 1;
                        if depth == 0 {
                            result = i as i32;
                            break;
                        }
                    }
                }
                Ok(Value::number(result as f64))
            }
            ["Text", "substring"] => {
                let s = input.as_text().unwrap_or("");
                let start = arguments
                    .iter()
                    .find(|(name, _)| name == "start")
                    .and_then(|(_, val_expr)| val_expr.as_ref())
                    .and_then(|v| self.eval_static(v).ok())
                    .and_then(|v| v.as_number())
                    .unwrap_or(0.0) as usize;
                let length = arguments
                    .iter()
                    .find(|(name, _)| name == "length")
                    .and_then(|(_, val_expr)| val_expr.as_ref())
                    .and_then(|v| self.eval_static(v).ok())
                    .and_then(|v| v.as_number())
                    .unwrap_or(0.0) as usize;
                let chars: Vec<char> = s.chars().collect();
                let end = (start + length).min(chars.len());
                let start = start.min(chars.len());
                let result: String = chars[start..end].iter().collect();
                Ok(Value::text(result))
            }
            ["Text", "to_uppercase"] => {
                let s = input.as_text().unwrap_or("");
                Ok(Value::text(s.to_uppercase()))
            }
            ["Math", "round"] => {
                let n = input.as_number().unwrap_or(0.0);
                Ok(Value::number(n.round()))
            }
            ["Math", "min"] => {
                let a = input.as_number().unwrap_or(0.0);
                let b = arguments
                    .iter()
                    .find(|(name, _)| name == "b")
                    .and_then(|(_, val_expr)| val_expr.as_ref())
                    .and_then(|v| self.eval_static(v).ok())
                    .and_then(|v| v.as_number())
                    .unwrap_or(0.0);
                Ok(Value::number(a.min(b)))
            }
            ["Math", "max"] => {
                let a = input.as_number().unwrap_or(0.0);
                let b = arguments
                    .iter()
                    .find(|(name, _)| name == "b")
                    .and_then(|(_, val_expr)| val_expr.as_ref())
                    .and_then(|v| self.eval_static(v).ok())
                    .and_then(|v| v.as_number())
                    .unwrap_or(0.0);
                Ok(Value::number(a.max(b)))
            }
            ["Math", "modulo"] => {
                let a = input.as_number().unwrap_or(0.0);
                let b = arguments
                    .iter()
                    .find(|(name, _)| name == "divisor")
                    .and_then(|(_, val_expr)| val_expr.as_ref())
                    .and_then(|v| self.eval_static(v).ok())
                    .and_then(|v| v.as_number())
                    .unwrap_or(1.0);
                Ok(Value::number(a % b))
            }
            ["List", "sum"] => {
                let items = input.list_items();
                let sum: f64 = items.iter().filter_map(|v| v.as_number()).sum();
                Ok(Value::number(sum))
            }
            ["List", "product"] => {
                let items = input.list_items();
                let prod: f64 = items.iter().filter_map(|v| v.as_number()).product();
                Ok(Value::number(prod))
            }
            ["List", "last"] => {
                let items = input.list_items();
                Ok(items.last().cloned().cloned().unwrap_or(Value::Unit))
            }
            ["List", "get"] => {
                let index = arguments
                    .iter()
                    .find(|(name, _)| name == "index")
                    .and_then(|(_, val_expr)| val_expr.as_ref())
                    .and_then(|v| self.eval_static(v).ok())
                    .and_then(|v| v.as_number())
                    .unwrap_or(1.0) as usize;
                // 1-based indexing
                let items = input.list_items();
                Ok(if index >= 1 && index <= items.len() {
                    items[index - 1].clone()
                } else {
                    Value::Unit
                })
            }
            _ => Err(format!("Not a built-in piped function: {}", path.join("/"))),
        }
    }

    fn register_top_level(&mut self, ast: &[Spanned<Expression>]) {
        for expr in ast {
            match &expr.node {
                Expression::Variable(var) => {
                    let name = var.name.as_str().to_string();
                    Arc::make_mut(&mut self.variables).push((name.clone(), var.value.clone()));
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
                    let params: Vec<String> = parameters
                        .iter()
                        .map(|p| p.node.as_str().to_string())
                        .collect();
                    Arc::make_mut(&mut self.functions).push((
                        fn_name,
                        params,
                        body.as_ref().clone(),
                    ));
                }
                _ => {}
            }
        }
    }

    /// Recursively flatten object fields into dotted-name variables.
    fn flatten_object_fields(&mut self, prefix: &str, expr: &Spanned<Expression>) {
        if let Expression::Object(obj) = &expr.node {
            for var in &obj.variables {
                if var.node.name.is_empty() {
                    // Spread entry: try to resolve the source's fields statically.
                    // Handles simple variable references like `...base` where `base`
                    // is a known object literal.
                    if let Expression::Alias(
                        crate::parser::static_expression::Alias::WithoutPassed { parts, .. },
                    ) = &var.node.value.node
                    {
                        if parts.len() == 1 {
                            let var_name = parts[0].as_str();
                            // Look up the variable's expression in the registered variables
                            if let Some(source_expr) = self
                                .variables
                                .iter()
                                .find(|(n, _)| n == var_name)
                                .map(|(_, e)| e.clone())
                            {
                                self.flatten_object_fields(prefix, &source_expr);
                            }
                        }
                    }
                    continue;
                }
                let field_name = format!("{}.{}", prefix, var.node.name.as_str());
                Arc::make_mut(&mut self.variables)
                    .push((field_name.clone(), var.node.value.clone()));
                // Recurse for nested objects
                self.flatten_object_fields(&field_name, &var.node.value);
            }
        }
    }

    /// Look up a variable expression by name.
    /// Uses rfind (last match) so that explicit fields override spread fields
    /// when both register the same dotted name.
    fn get_var_expr(&self, name: &str) -> Option<&Spanned<Expression>> {
        self.variables
            .iter()
            .rfind(|(n, _)| n == name)
            .map(|(_, e)| e)
    }

    fn has_reactive_constructs(&self) -> bool {
        // Only needs DataflowGraph if there are external inputs (LINK, Timer)
        // Programs with HOLD/WHILE but no external inputs (like fibonacci)
        // can be evaluated statically.
        let mut visiting_functions = HashSet::new();
        self.variables
            .iter()
            .any(|(_, expr)| self.has_external_input(expr, &mut visiting_functions))
    }

    fn has_external_input(
        &self,
        expr: &Spanned<Expression>,
        visiting_functions: &mut HashSet<String>,
    ) -> bool {
        match &expr.node {
            Expression::Link | Expression::LinkSetter { .. } => true,
            Expression::Variable(var) => self.has_external_input(&var.value, visiting_functions),
            Expression::Pipe { from, to } => {
                self.has_external_input(from, visiting_functions)
                    || self.has_external_input(to, visiting_functions)
            }
            Expression::FunctionCall { path, arguments } => {
                let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                if matches!(
                    path_strs.as_slice(),
                    ["Timer", "interval"] | ["Router", "route"] | ["Router", "go_to"]
                ) {
                    return true;
                }
                if arguments.iter().any(|a| {
                    a.node
                        .value
                        .as_ref()
                        .map(|v| self.has_external_input(v, visiting_functions))
                        .unwrap_or(false)
                }) {
                    return true;
                }
                let fn_name = match path_strs.as_slice() {
                    [fn_name] => Some((*fn_name).to_string()),
                    [module, fn_name] => Some(format!("{module}/{fn_name}")),
                    _ => None,
                };
                if let Some(fn_name) = fn_name {
                    if let Some((resolved_name, _, body)) = self.find_function(&fn_name) {
                        if let Some(cached) = self
                            .external_input_function_cache
                            .borrow()
                            .get(resolved_name.as_str())
                            .copied()
                        {
                            return cached;
                        }
                        if visiting_functions.insert(resolved_name.clone()) {
                            let has_external = self.has_external_input(body, visiting_functions);
                            visiting_functions.remove(resolved_name.as_str());
                            self.external_input_function_cache
                                .borrow_mut()
                                .insert(resolved_name.clone(), has_external);
                            return has_external;
                        }
                    }
                }
                false
            }
            Expression::List { items } => items
                .iter()
                .any(|item| self.has_external_input(item, visiting_functions)),
            Expression::Object(obj) => obj
                .variables
                .iter()
                .any(|v| self.has_external_input(&v.node.value, visiting_functions)),
            Expression::Block { variables, output } => {
                variables
                    .iter()
                    .any(|v| self.has_external_input(&v.node.value, visiting_functions))
                    || self.has_external_input(output, visiting_functions)
            }
            Expression::Hold { body, .. } => self.has_external_input(body, visiting_functions),
            Expression::Latest { inputs } => inputs
                .iter()
                .any(|input| self.has_external_input(input, visiting_functions)),
            Expression::Then { body } => self.has_external_input(body, visiting_functions),
            Expression::While { arms } => arms
                .iter()
                .any(|a| self.has_external_input(&a.body, visiting_functions)),
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
                } => {
                    self.has_external_input(operand_a, visiting_functions)
                        || self.has_external_input(operand_b, visiting_functions)
                }
                ArithmeticOperator::Negate { operand } => {
                    self.has_external_input(operand, visiting_functions)
                }
            },
            Expression::PostfixFieldAccess { expr, .. } => {
                self.has_external_input(expr, visiting_functions)
            }
            _ => false,
        }
    }

    fn is_reactive(&self, expr: &Spanned<Expression>) -> bool {
        let mut visiting_functions = HashSet::new();
        self.is_reactive_inner(expr, &mut visiting_functions)
    }

    fn is_reactive_inner(
        &self,
        expr: &Spanned<Expression>,
        visiting_functions: &mut HashSet<String>,
    ) -> bool {
        match &expr.node {
            Expression::Link | Expression::LinkSetter { .. } => true,
            Expression::Hold { .. } => true,
            Expression::Latest { .. } => true,
            Expression::Then { .. } => true,
            Expression::While { .. } => true,
            Expression::Variable(var) => self.is_reactive_inner(&var.value, visiting_functions),
            Expression::Pipe { from, to } => {
                self.is_reactive_or_alias_inner(from, visiting_functions)
                    || self.is_reactive_inner(to, visiting_functions)
            }
            Expression::When { .. } => true,
            Expression::FunctionCall { path, arguments } => {
                let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                if matches!(
                    path_strs.as_slice(),
                    ["Timer", "interval"]
                        | ["Stream", "skip"]
                        | ["Router", "route"]
                        | ["Router", "go_to"]
                        | ["List", "count"]
                        | ["List", "latest"]
                        | ["List", "every"]
                        | ["List", "any"]
                        | ["List", "is_not_empty"]
                        | ["List", "retain"]
                        | ["List", "map"]
                        | ["List", "append"]
                        | ["List", "clear"]
                        | ["List", "remove"]
                        | ["Bool", "toggle"]
                ) {
                    return true;
                }
                if arguments.iter().any(|a| {
                    a.node
                        .value
                        .as_ref()
                        .map(|v| self.is_reactive_inner(v, visiting_functions))
                        .unwrap_or(false)
                }) {
                    return true;
                }
                false
            }
            Expression::List { items } => items
                .iter()
                .any(|item| self.is_reactive_inner(item, visiting_functions)),
            Expression::Object(obj) => obj
                .variables
                .iter()
                .any(|v| self.is_reactive_inner(&v.node.value, visiting_functions)),
            Expression::Block { variables, output } => {
                variables
                    .iter()
                    .any(|v| self.is_reactive_inner(&v.node.value, visiting_functions))
                    || self.is_reactive_inner(output, visiting_functions)
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
                } => {
                    self.is_reactive_or_alias_inner(operand_a, visiting_functions)
                        || self.is_reactive_or_alias_inner(operand_b, visiting_functions)
                }
                ArithmeticOperator::Negate { operand } => {
                    self.is_reactive_or_alias_inner(operand, visiting_functions)
                }
            },
            Expression::Comparator(cmp) => {
                use static_expression::Comparator;
                let (a, b) = match cmp {
                    Comparator::Equal {
                        operand_a,
                        operand_b,
                    }
                    | Comparator::NotEqual {
                        operand_a,
                        operand_b,
                    }
                    | Comparator::Greater {
                        operand_a,
                        operand_b,
                    }
                    | Comparator::GreaterOrEqual {
                        operand_a,
                        operand_b,
                    }
                    | Comparator::Less {
                        operand_a,
                        operand_b,
                    }
                    | Comparator::LessOrEqual {
                        operand_a,
                        operand_b,
                    } => (operand_a, operand_b),
                };
                self.is_reactive_or_alias_inner(a, visiting_functions)
                    || self.is_reactive_or_alias_inner(b, visiting_functions)
            }
            Expression::TextLiteral { parts, .. } => parts
                .iter()
                .any(|p| matches!(p, TextPart::Interpolation { .. })),
            Expression::Alias(_) => {
                // An alias might reference a reactive variable — conservative.
                // But for top-level reactivity check, we look at definitions.
                false
            }
            Expression::PostfixFieldAccess { expr, .. } => {
                self.is_reactive_inner(expr, visiting_functions)
            }
            _ => false,
        }
    }

    /// Like `is_reactive`, but also returns true for aliases.
    /// Used for arithmetic/comparison operands where aliases likely reference
    /// sibling reactive vars (e.g., `todos_count - completed_todos_count`).
    fn is_reactive_or_alias_inner(
        &self,
        expr: &Spanned<Expression>,
        visiting_functions: &mut HashSet<String>,
    ) -> bool {
        matches!(&expr.node, Expression::Alias(_))
            || self.is_reactive_inner(expr, visiting_functions)
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
        use std::cell::Cell;
        thread_local! {
            static EVAL_DEPTH: Cell<usize> = const { Cell::new(0) };
        }
        const MAX_EVAL_DEPTH: usize = 150;

        let depth = EVAL_DEPTH.with(|d| {
            let current = d.get();
            d.set(current + 1);
            current
        });
        if depth >= MAX_EVAL_DEPTH {
            EVAL_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
            return Err("Maximum evaluation depth exceeded".to_string());
        }
        let result = self.eval_static_with_scope_inner(expr, local_scope);
        EVAL_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
        result
    }

    fn eval_static_with_scope_inner(
        &self,
        expr: &Spanned<Expression>,
        local_scope: &IndexMap<String, Value>,
    ) -> Result<Value, String> {
        match &expr.node {
            Expression::Literal(lit) => Ok(Self::eval_literal(lit)),

            Expression::TextLiteral { parts, .. } => {
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
                        val = val.get_field(part.as_str()).cloned().ok_or_else(|| {
                            format!("Field '{}' not found on {}", part.as_str(), val)
                        })?;
                    }
                    Ok(val)
                }
                Alias::WithPassed { extra_parts } => {
                    // Look up __passed in local scope
                    let passed = local_scope
                        .get(PASSED_VAR)
                        .ok_or_else(|| "PASSED not available in this context".to_string())?;
                    let mut val = passed.clone();
                    for part in extra_parts {
                        val = val.get_field(part.as_str()).cloned().ok_or_else(|| {
                            format!("Field '{}' not found on PASSED", part.as_str())
                        })?;
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
                    tag: Arc::from(LIST_TAG),
                    fields: Arc::new(fields),
                })
            }

            Expression::Object(obj) => {
                let mut fields = BTreeMap::new();
                // Build a sibling scope so that fields can reference previously-
                // evaluated siblings (e.g., `text_to_add` referencing `elements`
                // inside a `store` object).  Reactive fields that can't be
                // evaluated statically get a Unit placeholder — the document
                // closure replaces them with live values at runtime.
                let mut sibling_scope = local_scope.clone();
                for var in &obj.variables {
                    let name = var.node.name.as_str();
                    if name.is_empty() {
                        // Spread: evaluate expression, merge its fields
                        if let Ok(val) =
                            self.eval_static_with_scope(&var.node.value, &sibling_scope)
                        {
                            match val {
                                Value::Object(spread_fields) => {
                                    for (k, v) in spread_fields.iter() {
                                        sibling_scope.insert(k.to_string(), v.clone());
                                        fields.insert(k.clone(), v.clone());
                                    }
                                }
                                Value::Tagged {
                                    fields: spread_fields,
                                    ..
                                } => {
                                    for (k, v) in spread_fields.iter() {
                                        sibling_scope.insert(k.to_string(), v.clone());
                                        fields.insert(k.clone(), v.clone());
                                    }
                                }
                                _ => {} // Non-object spread ignored
                            }
                        }
                    } else {
                        match self.eval_static_with_scope(&var.node.value, &sibling_scope) {
                            Ok(val) => {
                                sibling_scope.insert(name.to_string(), val.clone());
                                fields.insert(Arc::from(name), val);
                            }
                            Err(_) => {
                                // Reactive field — use placeholder
                                fields.insert(Arc::from(name), Value::Unit);
                            }
                        }
                    }
                }
                Ok(Value::Object(Arc::new(fields)))
            }

            Expression::TaggedObject { tag, object } => {
                let mut fields = BTreeMap::new();
                for var in &object.variables {
                    let name = var.node.name.as_str();
                    if name.is_empty() {
                        let val = self.eval_static_with_scope(&var.node.value, local_scope)?;
                        match val {
                            Value::Object(spread_fields) => {
                                fields.extend(
                                    spread_fields.iter().map(|(k, v)| (k.clone(), v.clone())),
                                );
                            }
                            Value::Tagged {
                                fields: spread_fields,
                                ..
                            } => {
                                fields.extend(
                                    spread_fields.iter().map(|(k, v)| (k.clone(), v.clone())),
                                );
                            }
                            _ => {}
                        }
                    } else {
                        let val = self.eval_static_with_scope(&var.node.value, local_scope)?;
                        fields.insert(Arc::from(name), val);
                    }
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
            Expression::Spread { value } => self.eval_static_with_scope(value, local_scope),

            // HOLD in static context (non-piped): just return initial state
            Expression::Hold { .. } => {
                Err("HOLD requires pipe input in static context".to_string())
            }

            // THEN in static context (non-piped): evaluate body
            Expression::Then { body } => self.eval_static_with_scope(body, local_scope),

            // Postfix field access: evaluate expr, then extract field
            Expression::PostfixFieldAccess { expr, field } => {
                let val = self.eval_static_with_scope(expr, local_scope)?;
                val.get_field(field.as_str())
                    .cloned()
                    .ok_or_else(|| format!("Field '{}' not found", field.as_str()))
            }

            _ => Err(format!(
                "Unsupported expression in static eval: {:?}",
                std::mem::discriminant(&expr.node)
            )),
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
                    let name = var.node.name.as_str();
                    if name.is_empty() {
                        let val = self.eval_static_tolerant(&var.node.value, local_scope);
                        match val {
                            Value::Object(spread_fields) => {
                                fields.extend(
                                    spread_fields.iter().map(|(k, v)| (k.clone(), v.clone())),
                                );
                            }
                            Value::Tagged {
                                fields: spread_fields,
                                ..
                            } => {
                                fields.extend(
                                    spread_fields.iter().map(|(k, v)| (k.clone(), v.clone())),
                                );
                            }
                            _ => {}
                        }
                    } else {
                        let val = self.eval_static_tolerant(&var.node.value, local_scope);
                        fields.insert(Arc::from(name), val);
                    }
                }
                Value::Object(Arc::new(fields))
            }
            Expression::TaggedObject { tag, object } => {
                let mut fields = BTreeMap::new();
                for var in &object.variables {
                    let name = var.node.name.as_str();
                    if name.is_empty() {
                        let val = self.eval_static_tolerant(&var.node.value, local_scope);
                        match val {
                            Value::Object(spread_fields) => {
                                fields.extend(
                                    spread_fields.iter().map(|(k, v)| (k.clone(), v.clone())),
                                );
                            }
                            Value::Tagged {
                                fields: spread_fields,
                                ..
                            } => {
                                fields.extend(
                                    spread_fields.iter().map(|(k, v)| (k.clone(), v.clone())),
                                );
                            }
                            _ => {}
                        }
                    } else {
                        let val = self.eval_static_tolerant(&var.node.value, local_scope);
                        fields.insert(Arc::from(name), val);
                    }
                }
                Value::Tagged {
                    tag: Arc::from(tag.as_str()),
                    fields: Arc::new(fields),
                }
            }
            Expression::Block {
                variables, output, ..
            } => {
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
                    let _ = state_param;
                    let _ = body;
                    self.eval_static_tolerant(from, local_scope)
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
                                if let Some(bindings) = self.match_pattern(&from_val, &arm.pattern)
                                {
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
                        Expression::Then { body } => self.eval_static_tolerant(body, local_scope),
                        Expression::FunctionCall { path, arguments } => {
                            // Re-evaluate pipe with tolerant from_val
                            // Build a temporary scope with the from_val
                            let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                            match path_strs.as_slice() {
                                ["List", "map"] => {
                                    let item_param = arguments
                                        .first()
                                        .map(|a| a.node.name.as_str().to_string())
                                        .unwrap_or_else(|| "item".to_string());
                                    let map_expr = arguments
                                        .iter()
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
                                    let item_param = arguments
                                        .first()
                                        .map(|a| a.node.name.as_str().to_string())
                                        .unwrap_or_else(|| "item".to_string());
                                    let if_expr = arguments
                                        .iter()
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
                                // Piped user function: `value |> fn_name()`
                                [fn_name] => {
                                    if let Some((qualified_name, params, body)) =
                                        self.find_function(fn_name)
                                    {
                                        let scoped = self.with_module_context(&qualified_name);
                                        let mut fn_scope = local_scope.clone();
                                        if let Some(first_param) = params.first() {
                                            fn_scope.insert(first_param.clone(), from_val.clone());
                                        }
                                        for arg in arguments {
                                            let arg_name = arg.node.name.as_str();
                                            if arg_name == "PASS" {
                                                if let Some(ref val_expr) = arg.node.value {
                                                    let val = self.eval_static_tolerant(
                                                        val_expr,
                                                        local_scope,
                                                    );
                                                    fn_scope.insert(PASSED_VAR.to_string(), val);
                                                }
                                                continue;
                                            }
                                            if let Some(ref val_expr) = arg.node.value {
                                                let val = self
                                                    .eval_static_tolerant(val_expr, local_scope);
                                                fn_scope.insert(arg_name.to_string(), val);
                                            }
                                        }
                                        scoped.eval_static_tolerant(&body, &fn_scope)
                                    } else {
                                        from_val
                                    }
                                }
                                // Module-qualified piped function: `value |> Module/fn()`
                                [module, fn_name] => {
                                    let qualified = format!("{}/{}", module, fn_name);
                                    if let Some((qualified_name, params, body)) =
                                        self.find_function(&qualified)
                                    {
                                        let scoped = self.with_module_context(&qualified_name);
                                        let mut fn_scope = local_scope.clone();
                                        if let Some(first_param) = params.first() {
                                            fn_scope.insert(first_param.clone(), from_val.clone());
                                        }
                                        for arg in arguments {
                                            let arg_name = arg.node.name.as_str();
                                            if arg_name == "PASS" {
                                                if let Some(ref val_expr) = arg.node.value {
                                                    let val = self.eval_static_tolerant(
                                                        val_expr,
                                                        local_scope,
                                                    );
                                                    fn_scope.insert(PASSED_VAR.to_string(), val);
                                                }
                                                continue;
                                            }
                                            if let Some(ref val_expr) = arg.node.value {
                                                let val = self
                                                    .eval_static_tolerant(val_expr, local_scope);
                                                fn_scope.insert(arg_name.to_string(), val);
                                            }
                                        }
                                        scoped.eval_static_tolerant(&body, &fn_scope)
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
                                val = val
                                    .get_field(field.as_str())
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
                    tag: Arc::from(LIST_TAG),
                    fields: Arc::new(fields),
                }
            }
            Expression::FunctionCall { path, arguments } => {
                let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                match path_strs.as_slice() {
                    // User-defined function: use tolerant evaluation directly to avoid
                    // recursing through reactive aliases before the fallback gets a chance.
                    [fn_name] => {
                        return self.eval_user_function_tolerant(fn_name, arguments, local_scope);
                    }
                    [module, fn_name] => {
                        let qualified = format!("{}/{}", module, fn_name);
                        if self.find_function(&qualified).is_some() {
                            return self.eval_user_function_tolerant(
                                &qualified,
                                arguments,
                                local_scope,
                            );
                        }
                    }
                    _ => {}
                }
                // Try strict eval first for builtins and known pure calls.
                if let Ok(val) = self.eval_function_call_static(path, arguments, local_scope) {
                    return val;
                }
                // Tolerant fallback based on function type
                match path_strs.as_slice() {
                    // Scene/new: preserve root, lights, and geometry for later renderer use.
                    ["Scene", "new"] => {
                        let mut fields = BTreeMap::new();
                        for arg in arguments {
                            let name = arg.node.name.as_str();
                            if matches!(name, "root" | "lights" | "geometry") {
                                if let Some(ref val_expr) = arg.node.value {
                                    fields.insert(
                                        Arc::from(name),
                                        self.eval_static_tolerant(val_expr, local_scope),
                                    );
                                }
                            }
                        }
                        return Value::Tagged {
                            tag: Arc::from("SceneNew"),
                            fields: Arc::new(fields),
                        };
                    }
                    ["Lights", "basic"] | ["Lights", "directional"] | ["Lights", "ambient"] => {
                        return Value::empty_list();
                    }
                    ["Light", "directional"] => {
                        let mut fields = BTreeMap::new();
                        for arg in arguments {
                            let name = arg.node.name.as_str();
                            if matches!(
                                name,
                                "azimuth" | "altitude" | "spread" | "intensity" | "color"
                            ) {
                                if let Some(ref val_expr) = arg.node.value {
                                    fields.insert(
                                        Arc::from(name),
                                        self.eval_static_tolerant(val_expr, local_scope),
                                    );
                                }
                            }
                        }
                        return Value::Tagged {
                            tag: Arc::from("DirectionalLight"),
                            fields: Arc::new(fields),
                        };
                    }
                    ["Light", "ambient"] => {
                        let mut fields = BTreeMap::new();
                        for arg in arguments {
                            let name = arg.node.name.as_str();
                            if matches!(name, "intensity" | "color") {
                                if let Some(ref val_expr) = arg.node.value {
                                    fields.insert(
                                        Arc::from(name),
                                        self.eval_static_tolerant(val_expr, local_scope),
                                    );
                                }
                            }
                        }
                        return Value::Tagged {
                            tag: Arc::from("AmbientLight"),
                            fields: Arc::new(fields),
                        };
                    }
                    ["Light", "spot"] => {
                        let mut fields = BTreeMap::new();
                        for arg in arguments {
                            let name = arg.node.name.as_str();
                            if matches!(
                                name,
                                "target" | "color" | "intensity" | "radius" | "softness"
                            ) {
                                if let Some(ref val_expr) = arg.node.value {
                                    fields.insert(
                                        Arc::from(name),
                                        self.eval_static_tolerant(val_expr, local_scope),
                                    );
                                }
                            }
                        }
                        return Value::Tagged {
                            tag: Arc::from("SpotLight"),
                            fields: Arc::new(fields),
                        };
                    }
                    // Scene/Element/* — aliases for Element/*
                    ["Scene", "Element", kind] | ["Element", kind] => {
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
                        let mut has_hovered_link = false;
                        if let Some(el_arg) =
                            arguments.iter().find(|a| a.node.name.as_str() == "element")
                        {
                            if let Some(ref val_expr) = el_arg.node.value {
                                let el_val = self.eval_static_tolerant(val_expr, local_scope);
                                // Check for hovered: LINK and resolve hover state from scope
                                has_hovered_link = el_val
                                    .get_field("hovered")
                                    .map(|v| v.as_tag() == Some("LINK"))
                                    .unwrap_or(false);
                                if has_hovered_link {
                                    let hover_state = local_scope
                                        .values()
                                        .find_map(|v| v.get_field(HOVERED_FIELD))
                                        .cloned()
                                        .unwrap_or(Value::tag("False"));
                                    hover_path = local_scope.values().find_map(|v| {
                                        v.get_field(HOVER_PATH_FIELD)
                                            .and_then(|p| p.as_text().map(|s| s.to_string()))
                                    });
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
                            fields.insert(Arc::from(HOVER_PATH_FIELD), Value::text(path.as_str()));
                        }
                        // If element has hovered: LINK, also evaluate style with hovered=True
                        // and store as __style_hovered__ so the bridge can create hover signals.
                        if has_hovered_link {
                            let mut hovered_scope = elem_scope.clone();
                            if let Some(el_val) = hovered_scope.get("element").cloned() {
                                hovered_scope.insert(
                                    "element".to_string(),
                                    el_val.update_field("hovered", Value::tag("True")),
                                );
                            }
                            if let Some(style_arg) =
                                arguments.iter().find(|a| a.node.name.as_str() == "style")
                            {
                                if let Some(ref val_expr) = style_arg.node.value {
                                    let hovered_style =
                                        self.eval_static_tolerant(val_expr, &hovered_scope);
                                    fields.insert(Arc::from("__style_hovered__"), hovered_style);
                                }
                            }
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
                    _ => Value::Unit,
                }
            }
            // Alias: try strict eval with special handling for element self-references
            Expression::Alias(alias) => {
                if let Alias::WithoutPassed { parts, .. } = alias {
                    let path = parts
                        .iter()
                        .map(|p| p.as_str())
                        .collect::<Vec<_>>()
                        .join(".");
                    if path.contains(".event.") {
                        if let Ok(val) = self.eval_static_with_scope(expr, local_scope) {
                            return val;
                        }
                        return Value::Unit;
                    }
                    let has_local_binding = local_scope.contains_key(path.as_str())
                        || parts
                            .first()
                            .map(|part| local_scope.contains_key(part.as_str()))
                            .unwrap_or(false);
                    if has_local_binding {
                        if let Ok(val) = self.eval_static_with_scope(expr, local_scope) {
                            return val;
                        }
                    }
                    if let Some(var_expr) = self.get_var_expr(&path) {
                        if self.is_reactive(var_expr) || self.expr_contains_link(var_expr) {
                            return Value::Unit;
                        }
                    }
                }
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
            // Comparators: evaluate both operands tolerantly and compare.
            // When an operand evaluates to Unit (reactive/unavailable), default to False
            // for equality and True for inequality (assume unknown values differ).
            Expression::Comparator(cmp) => {
                if let Ok(val) = self.eval_comparator_static(cmp, local_scope) {
                    return val;
                }
                use static_expression::Comparator;
                let (a_expr, b_expr, is_equality) = match cmp {
                    Comparator::Equal {
                        operand_a,
                        operand_b,
                    } => (operand_a, operand_b, true),
                    Comparator::NotEqual {
                        operand_a,
                        operand_b,
                    } => (operand_a, operand_b, false),
                    Comparator::Greater {
                        operand_a,
                        operand_b,
                    }
                    | Comparator::GreaterOrEqual {
                        operand_a,
                        operand_b,
                    }
                    | Comparator::Less {
                        operand_a,
                        operand_b,
                    }
                    | Comparator::LessOrEqual {
                        operand_a,
                        operand_b,
                    } => (operand_a, operand_b, false),
                };
                let a = self.eval_static_tolerant(a_expr, local_scope);
                let b = self.eval_static_tolerant(b_expr, local_scope);
                if matches!(&a, Value::Unit) || matches!(&b, Value::Unit) {
                    // Can't evaluate one side — default to "not equal"
                    if is_equality {
                        Value::tag("False")
                    } else {
                        Value::tag("True")
                    }
                } else if is_equality {
                    if a == b {
                        Value::tag("True")
                    } else {
                        Value::tag("False")
                    }
                } else {
                    // For ordering comparisons with both values available, re-try strict
                    self.eval_comparator_static(cmp, local_scope)
                        .unwrap_or(Value::tag("False"))
                }
            }
            // For anything else, try strict eval and fall back to Unit
            _ => self
                .eval_static_with_scope(expr, local_scope)
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
        let (qualified_name, params, body) = match self.find_function(fn_name) {
            Some(f) => f,
            None => return Value::Unit,
        };

        // Set up module context for intra-module resolution in the function body
        let scoped = self.with_module_context(&qualified_name);
        let mut fn_scope = local_scope.clone();

        for arg in arguments {
            let arg_name = arg.node.name.as_str();
            if arg_name == "PASS" {
                if let Some(ref val_expr) = arg.node.value {
                    let val = self.eval_static_tolerant(val_expr, local_scope);
                    fn_scope.insert(PASSED_VAR.to_string(), val);
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

        scoped.eval_static_tolerant(&body, &fn_scope)
    }

    fn populate_scope_bindings_tolerant(
        &self,
        scope: &mut IndexMap<String, Value>,
        bindings: &[(String, Spanned<Expression>)],
    ) {
        for (name, expr) in bindings {
            let value = self.eval_static_tolerant(expr, scope);
            scope.insert(name.clone(), value);
        }
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
        // Support dotted aliases that walk fields from a locally bound object,
        // e.g. `person.surname` inside keyed List/map display functions.
        if let Some((base, rest)) = name.split_once('.') {
            if let Some(mut value) = local_scope.get(base).cloned() {
                for field in rest.split('.') {
                    value = value
                        .get_field(field)
                        .cloned()
                        .ok_or_else(|| format!("Field '{}' not found in '{}'", field, name))?;
                }
                return Ok(value);
            }
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

            // Scene/new: preserve root, lights, and geometry for later renderer use.
            ["Scene", "new"] => {
                let mut fields = BTreeMap::new();
                for arg in arguments {
                    let name = arg.node.name.as_str();
                    if matches!(name, "root" | "lights" | "geometry") {
                        if let Some(ref val_expr) = arg.node.value {
                            let val = self.eval_static_with_scope(val_expr, local_scope)?;
                            fields.insert(Arc::from(name), val);
                        }
                    }
                }
                if fields.contains_key("root") {
                    Ok(Value::Tagged {
                        tag: Arc::from("SceneNew"),
                        fields: Arc::new(fields),
                    })
                } else {
                    Err("Scene/new requires a 'root' argument".to_string())
                }
            }

            ["Lights", "basic"] | ["Lights", "directional"] | ["Lights", "ambient"] => {
                Ok(Value::empty_list())
            }
            ["Light", "directional"] => {
                let mut fields = BTreeMap::new();
                for arg in arguments {
                    let name = arg.node.name.as_str();
                    if matches!(
                        name,
                        "azimuth" | "altitude" | "spread" | "intensity" | "color"
                    ) {
                        if let Some(ref val_expr) = arg.node.value {
                            let val = self.eval_static_with_scope(val_expr, local_scope)?;
                            fields.insert(Arc::from(name), val);
                        }
                    }
                }
                Ok(Value::Tagged {
                    tag: Arc::from("DirectionalLight"),
                    fields: Arc::new(fields),
                })
            }
            ["Light", "ambient"] => {
                let mut fields = BTreeMap::new();
                for arg in arguments {
                    let name = arg.node.name.as_str();
                    if matches!(name, "intensity" | "color") {
                        if let Some(ref val_expr) = arg.node.value {
                            let val = self.eval_static_with_scope(val_expr, local_scope)?;
                            fields.insert(Arc::from(name), val);
                        }
                    }
                }
                Ok(Value::Tagged {
                    tag: Arc::from("AmbientLight"),
                    fields: Arc::new(fields),
                })
            }
            ["Light", "spot"] => {
                let mut fields = BTreeMap::new();
                for arg in arguments {
                    let name = arg.node.name.as_str();
                    if matches!(
                        name,
                        "target" | "color" | "intensity" | "radius" | "softness"
                    ) {
                        if let Some(ref val_expr) = arg.node.value {
                            let val = self.eval_static_with_scope(val_expr, local_scope)?;
                            fields.insert(Arc::from(name), val);
                        }
                    }
                }
                Ok(Value::Tagged {
                    tag: Arc::from("SpotLight"),
                    fields: Arc::new(fields),
                })
            }

            // Scene/Element/* — aliases for Element/*
            ["Scene", "Element", kind] => {
                let tag = match *kind {
                    "button" => "ElementButton",
                    "stripe" => "ElementStripe",
                    "container" => "ElementContainer",
                    "stack" => "ElementStack",
                    "paragraph" => "ElementParagraph",
                    "text_input" => "ElementTextInput",
                    "label" => "ElementLabel",
                    "link" => "ElementLink",
                    "checkbox" => "ElementCheckbox",
                    "block" => "ElementBlock",
                    "text" => "ElementText",
                    _ => return Err(format!("Unknown Scene/Element type: {}", kind)),
                };
                self.eval_element_static(tag, arguments, local_scope)
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

            ["Element", "link"] => self.eval_element_static("ElementLink", arguments, local_scope),

            ["Element", "checkbox"] => {
                self.eval_element_static("ElementCheckbox", arguments, local_scope)
            }

            ["Element", "text"] => self.eval_element_static("ElementText", arguments, local_scope),

            ["Element", "block"] => {
                self.eval_element_static("ElementBlock", arguments, local_scope)
            }

            ["Math", "sum"] => {
                // Static Math/sum — meaningless without reactive input
                Ok(Value::number(0.0))
            }

            // Ulid
            ["Ulid", "generate"] => {
                let id = ulid::Ulid::new();
                Ok(Value::text(id.to_string()))
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
            ["List", "range"] => {
                let from = arguments
                    .iter()
                    .find(|a| a.node.name.as_str() == "from")
                    .and_then(|a| a.node.value.as_ref())
                    .and_then(|v| self.eval_static_with_scope(v, local_scope).ok())
                    .and_then(|v| v.as_number())
                    .unwrap_or(1.0) as i64;
                let to = arguments
                    .iter()
                    .find(|a| a.node.name.as_str() == "to")
                    .and_then(|a| a.node.value.as_ref())
                    .and_then(|v| self.eval_static_with_scope(v, local_scope).ok())
                    .and_then(|v| v.as_number())
                    .unwrap_or(1.0) as i64;
                let mut fields = std::collections::BTreeMap::new();
                for i in from..=to {
                    let key: Arc<str> = Arc::from(format!("{:04}", i - from));
                    fields.insert(key, Value::number(i as f64));
                }
                Ok(Value::Tagged {
                    tag: Arc::from(super::types::LIST_TAG),
                    fields: Arc::new(fields),
                })
            }
            ["List", "sum"] => Ok(Value::number(0.0)),
            ["List", "product"] => Ok(Value::number(1.0)),
            ["List", "last"] => Ok(Value::Unit),
            ["List", "get"] => Ok(Value::Unit),

            // New element types for 7GUIs
            ["Element", "select"] => {
                self.eval_element_static("ElementSelect", arguments, local_scope)
            }
            ["Element", "slider"] => {
                self.eval_element_static("ElementSlider", arguments, local_scope)
            }
            ["Element", "svg"] => self.eval_element_static("ElementSvg", arguments, local_scope),
            ["Element", "svg_circle"] => {
                self.eval_element_static("ElementSvgCircle", arguments, local_scope)
            }

            // New text/math functions (non-piped forms)
            ["Text", "to_number"] => Ok(Value::number(0.0)),
            ["Text", "starts_with"] => Ok(Value::tag("False")),
            ["Text", "length"] => Ok(Value::number(0.0)),
            ["Text", "char_at"] => Ok(Value::text("")),
            ["Text", "char_code"] => Ok(Value::number(0.0)),
            ["Text", "from_char_code"] => Ok(Value::text("")),
            ["Text", "find"] => Ok(Value::number(-1.0)),
            ["Text", "find_closing"] => Ok(Value::number(-1.0)),
            ["Text", "substring"] => Ok(Value::text("")),
            ["Text", "to_uppercase"] => Ok(Value::text("")),
            ["Math", "round"] => Ok(Value::number(0.0)),
            ["Math", "min"] => Ok(Value::number(0.0)),
            ["Math", "max"] => Ok(Value::number(0.0)),
            ["Math", "modulo"] => Ok(Value::number(0.0)),

            [fn_name] => {
                // User-defined function call
                self.eval_user_function_static(fn_name, arguments, local_scope)
            }

            // Module-qualified function: Theme/material(), Professional/get(), etc.
            [module, fn_name] => {
                let qualified = format!("{}/{}", module, fn_name);
                self.eval_user_function_static(&qualified, arguments, local_scope)
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
        let mut has_hovered_link = false;
        if let Some(el_arg) = arguments.iter().find(|a| a.node.name.as_str() == "element") {
            if let Some(ref val_expr) = el_arg.node.value {
                let el_val = self.eval_static_with_scope(val_expr, local_scope)?;
                // Check if element has hovered: LINK and resolve hover state from scope
                has_hovered_link = el_val
                    .get_field("hovered")
                    .map(|v| v.as_tag() == Some("LINK"))
                    .unwrap_or(false);
                if has_hovered_link {
                    // Look for __hovered state from any scope variable (from list item)
                    let hover_state = local_scope
                        .values()
                        .find_map(|v| v.get_field(HOVERED_FIELD))
                        .cloned()
                        .unwrap_or(Value::tag("False"));
                    // Look for __hover_path__ from any scope variable
                    hover_path = local_scope.values().find_map(|v| {
                        v.get_field(HOVER_PATH_FIELD)
                            .and_then(|p| p.as_text().map(|s| s.to_string()))
                    });
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
        // Detect keyed Stripe: scope carries __keyed_list_name__ from the document
        // closure when a keyed list is active. Check if this Stripe's items expression
        // references PASSED.store.<keyed_list_name>, meaning its items come from the
        // DD keyed list and should be managed via keyed diffs, not the Value tree.
        let is_keyed_stripe = tag == "ElementStripe"
            && local_scope
                .get(KEYED_LIST_NAME_FIELD)
                .and_then(|v| v.as_text())
                .map(|keyed_name| {
                    arguments
                        .iter()
                        .find(|a| a.node.name.as_str() == "items")
                        .and_then(|items_arg| items_arg.node.value.as_ref())
                        .map(|val_expr| self.expr_references_passed_store(val_expr, keyed_name))
                        .unwrap_or(false)
                })
                .unwrap_or(false);

        let mut fields = BTreeMap::new();
        for arg in arguments {
            let name = arg.node.name.as_str();
            if let Some(ref val_expr) = arg.node.value {
                // For keyed Stripes, skip evaluating items entirely — the bridge
                // populates them from keyed diffs. This is both correct (avoids
                // stale data in the Value tree) and efficient (no per-item eval).
                if is_keyed_stripe && name == "items" {
                    fields.insert(Arc::from("items"), Value::empty_list());
                    continue;
                }
                let val = self.eval_static_with_scope(val_expr, &elem_scope)?;
                fields.insert(Arc::from(name), val);
            }
        }
        if is_keyed_stripe {
            fields.insert(Arc::from("__keyed__"), Value::tag("True"));
        }
        // Inject __hover_path__ for the bridge to extract
        if let Some(ref path) = hover_path {
            fields.insert(Arc::from(HOVER_PATH_FIELD), Value::text(path.as_str()));
        }
        // If element has hovered: LINK, also evaluate style with hovered=True
        // and store as __style_hovered__ so the bridge can create hover signals.
        // Check has_hovered_link (not hover_path) because non-list elements get their
        // hover path from __link_path__ (LINK pipe), not from scope's __hover_path__.
        if has_hovered_link {
            let mut hovered_scope = elem_scope.clone();
            if let Some(el_val) = hovered_scope.get("element").cloned() {
                hovered_scope.insert(
                    "element".to_string(),
                    el_val.update_field("hovered", Value::tag("True")),
                );
            }
            if let Some(style_arg) = arguments.iter().find(|a| a.node.name.as_str() == "style") {
                if let Some(ref val_expr) = style_arg.node.value {
                    if let Ok(hovered_style) = self.eval_static_with_scope(val_expr, &hovered_scope)
                    {
                        fields.insert(Arc::from("__style_hovered__"), hovered_style);
                    }
                }
            }
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
        let (qualified_name, params, body) = self
            .find_function(fn_name)
            .ok_or_else(|| format!("Function '{}' not found", fn_name))?;

        // Set up module context for intra-module resolution in the function body
        let scoped = self.with_module_context(&qualified_name);

        let mut fn_scope = local_scope.clone();

        // Handle PASS argument — sets __passed context for the function
        for arg in arguments {
            let arg_name = arg.node.name.as_str();
            if arg_name == "PASS" {
                if let Some(ref val_expr) = arg.node.value {
                    let val = self.eval_static_with_scope(val_expr, local_scope)?;
                    fn_scope.insert(PASSED_VAR.to_string(), val);
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

        scoped.eval_static_with_scope(&body, &fn_scope)
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
                    ["Scene", "new"] => {
                        if arguments.is_empty() {
                            Ok(Value::tagged("SceneNew", [("root", from_val)]))
                        } else {
                            self.eval_function_call_static(path, arguments, local_scope)
                        }
                    }
                    ["Stream", "skip"] => {
                        let count = arguments
                            .iter()
                            .find(|a| a.node.name.as_str() == "count")
                            .and_then(|a| a.node.value.as_ref())
                            .and_then(|v| self.eval_static_with_scope(v, local_scope).ok())
                            .and_then(|v| v.as_number())
                            .unwrap_or(0.0) as usize;

                        if let Value::Tagged { tag, fields } = &from_val {
                            if tag.as_ref() == LIST_TAG {
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
                            tag: Arc::from(LIST_TAG),
                            fields: Arc::new(fields),
                        })
                    }
                    ["Log", "info"] => Ok(from_val),

                    // Text utilities (piped)
                    ["Text", "trim"] => {
                        let s = from_val.as_text().unwrap_or("");
                        Ok(Value::text(s.trim()))
                    }
                    ["Text", "is_not_empty"] => {
                        let s = from_val.as_text().unwrap_or("");
                        Ok(if !s.is_empty() {
                            Value::tag("True")
                        } else {
                            Value::tag("False")
                        })
                    }
                    ["Text", "is_empty"] => {
                        let s = from_val.as_text().unwrap_or("");
                        Ok(if s.is_empty() {
                            Value::tag("True")
                        } else {
                            Value::tag("False")
                        })
                    }

                    // Bool utilities (piped)
                    ["Bool", "not"] => {
                        let b = from_val.as_bool().unwrap_or(false);
                        Ok(if b {
                            Value::tag("False")
                        } else {
                            Value::tag("True")
                        })
                    }
                    ["Bool", "or"] => {
                        let a = from_val.as_bool().unwrap_or(false);
                        let b = arguments
                            .iter()
                            .find(|arg| arg.node.name.as_str() == "that")
                            .and_then(|arg| arg.node.value.as_ref())
                            .and_then(|v| self.eval_static_with_scope(v, local_scope).ok())
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        Ok(if a || b {
                            Value::tag("True")
                        } else {
                            Value::tag("False")
                        })
                    }
                    ["Bool", "and"] => {
                        let a = from_val.as_bool().unwrap_or(false);
                        let b = arguments
                            .iter()
                            .find(|arg| arg.node.name.as_str() == "that")
                            .and_then(|arg| arg.node.value.as_ref())
                            .and_then(|v| self.eval_static_with_scope(v, local_scope).ok())
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        Ok(if a && b {
                            Value::tag("True")
                        } else {
                            Value::tag("False")
                        })
                    }

                    // Bool/toggle in static context — just return the initial value
                    ["Bool", "toggle"] => Ok(from_val),

                    // List utilities (piped)
                    ["List", "count"] => Ok(Value::number(from_val.list_count() as f64)),
                    ["List", "latest"] => {
                        // Static: return last list element
                        if let Value::Tagged { tag, fields } = &from_val {
                            if tag.as_ref() == LIST_TAG {
                                if let Some(last) = fields.values().last() {
                                    return Ok(last.clone());
                                }
                            }
                        }
                        Ok(Value::Unit)
                    }
                    ["List", "every"] => {
                        // Static: check all items match predicate
                        let item_param = arguments
                            .first()
                            .map(|a| a.node.name.as_str().to_string())
                            .unwrap_or_else(|| "item".to_string());
                        let if_expr = arguments
                            .iter()
                            .find(|a| a.node.name.as_str() == "if")
                            .and_then(|a| a.node.value.as_ref());
                        if let Some(pred) = if_expr {
                            let pred = pred.clone();
                            let all = from_val.list_every(|item_val| {
                                let mut scope = local_scope.clone();
                                scope.insert(item_param.clone(), item_val.clone());
                                self.eval_static_tolerant(&pred, &scope)
                                    .as_bool()
                                    .unwrap_or(false)
                            });
                            Ok(if all {
                                Value::tag("True")
                            } else {
                                Value::tag("False")
                            })
                        } else {
                            Ok(Value::tag("True"))
                        }
                    }
                    ["List", "any"] => {
                        // Static: check any item matches predicate
                        let item_param = arguments
                            .first()
                            .map(|a| a.node.name.as_str().to_string())
                            .unwrap_or_else(|| "item".to_string());
                        let if_expr = arguments
                            .iter()
                            .find(|a| a.node.name.as_str() == "if")
                            .and_then(|a| a.node.value.as_ref());
                        if let Some(pred) = if_expr {
                            let pred = pred.clone();
                            let any = from_val.list_any(|item_val| {
                                let mut scope = local_scope.clone();
                                scope.insert(item_param.clone(), item_val.clone());
                                self.eval_static_tolerant(&pred, &scope)
                                    .as_bool()
                                    .unwrap_or(false)
                            });
                            Ok(if any {
                                Value::tag("True")
                            } else {
                                Value::tag("False")
                            })
                        } else {
                            Ok(Value::tag("False"))
                        }
                    }
                    ["List", "is_not_empty"] => Ok(if !from_val.list_is_empty() {
                        Value::tag("True")
                    } else {
                        Value::tag("False")
                    }),
                    ["List", "is_empty"] => Ok(if from_val.list_is_empty() {
                        Value::tag("True")
                    } else {
                        Value::tag("False")
                    }),
                    ["List", "map"] => {
                        // `list |> List/map(item, new: expr)`
                        // Extract the item parameter name and mapping expression
                        let item_param = arguments
                            .first()
                            .map(|a| a.node.name.as_str().to_string())
                            .unwrap_or_else(|| "item".to_string());
                        let map_expr = arguments
                            .iter()
                            .find(|a| a.node.name.as_str() == "new")
                            .and_then(|a| a.node.value.as_ref());
                        if let Some(expr) = map_expr {
                            let expr = expr.clone();
                            let mapped = from_val.list_map(|item_val| {
                                let mut map_scope = local_scope.clone();
                                map_scope.insert(item_param.clone(), item_val.clone());
                                self.eval_static_with_scope(&expr, &map_scope)
                                    .unwrap_or_else(|_| {
                                        self.eval_static_tolerant(&expr, &map_scope)
                                    })
                            });
                            Ok(mapped)
                        } else {
                            Ok(from_val)
                        }
                    }
                    ["List", "append"] => {
                        let item = arguments
                            .iter()
                            .find(|a| {
                                a.node.name.as_str() == "item" || a.node.name.as_str() == "on"
                            })
                            .and_then(|a| a.node.value.as_ref())
                            .map(|expr| self.eval_static_with_scope(expr, local_scope))
                            .transpose()?
                            .unwrap_or(Value::Unit);
                        let count = from_val.list_count();
                        Ok(from_val.list_append(item, count))
                    }
                    ["List", "clear"] => Ok(Value::empty_list()),
                    ["List", "remove"] => {
                        let item_param = arguments
                            .first()
                            .map(|a| a.node.name.as_str().to_string())
                            .unwrap_or_else(|| "item".to_string());
                        let if_expr = arguments
                            .iter()
                            .find(|a| a.node.name.as_str() == "if")
                            .and_then(|a| a.node.value.as_ref());
                        if let Some(pred) = if_expr {
                            let pred = pred.clone();
                            let retained = from_val.list_retain(|item_val| {
                                let mut pred_scope = local_scope.clone();
                                pred_scope.insert(item_param.clone(), item_val.clone());
                                !self
                                    .eval_static_tolerant(&pred, &pred_scope)
                                    .as_bool()
                                    .unwrap_or(false)
                            });
                            Ok(retained)
                        } else {
                            Ok(from_val)
                        }
                    }
                    ["List", "retain"] => {
                        // `list |> List/retain(item, if: predicate_expr)`
                        let item_param = arguments
                            .first()
                            .map(|a| a.node.name.as_str().to_string())
                            .unwrap_or_else(|| "item".to_string());
                        let if_expr = arguments
                            .iter()
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
                        if let Some((qualified_name, params, body)) = self.find_function(fn_name) {
                            let scoped = self.with_module_context(&qualified_name);
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
                            return scoped.eval_static_with_scope(&body, &fn_scope);
                        }
                        Err(format!("Unknown piped function: {}", fn_name))
                    }
                    // Module-qualified piped function: builtins first, then user functions
                    [module, fn_name] => {
                        // Built-in Text/ functions
                        if *module == "Text" {
                            match *fn_name {
                                "trim" => {
                                    let s = from_val.as_text().unwrap_or("");
                                    return Ok(Value::text(s.trim()));
                                }
                                "to_number" => {
                                    let s = from_val.as_text().unwrap_or("");
                                    return match s.trim().parse::<f64>() {
                                        Ok(n) if n.is_finite() => Ok(Value::number(n)),
                                        _ => Ok(Value::tag("NaN")),
                                    };
                                }
                                "starts_with" => {
                                    let s = from_val.as_text().unwrap_or("");
                                    let prefix = arguments
                                        .iter()
                                        .find(|a| a.node.name.as_str() == "prefix")
                                        .and_then(|a| a.node.value.as_ref())
                                        .and_then(|v| {
                                            self.eval_static_with_scope(v, local_scope).ok()
                                        })
                                        .and_then(|v| v.as_text().map(|s| s.to_string()))
                                        .unwrap_or_default();
                                    return Ok(if s.starts_with(&prefix) {
                                        Value::tag("True")
                                    } else {
                                        Value::tag("False")
                                    });
                                }
                                "length" => {
                                    let s = from_val.as_text().unwrap_or("");
                                    return Ok(Value::number(s.len() as f64));
                                }
                                "to_uppercase" => {
                                    let s = from_val.as_text().unwrap_or("");
                                    return Ok(Value::text(s.to_uppercase()));
                                }
                                "char_at" => {
                                    let s = from_val.as_text().unwrap_or("");
                                    let idx = arguments
                                        .iter()
                                        .find(|a| a.node.name.as_str() == "index")
                                        .and_then(|a| a.node.value.as_ref())
                                        .and_then(|v| {
                                            self.eval_static_with_scope(v, local_scope).ok()
                                        })
                                        .and_then(|v| v.as_number())
                                        .unwrap_or(0.0)
                                        as usize;
                                    return Ok(s
                                        .chars()
                                        .nth(idx)
                                        .map(|c| Value::text(c.to_string()))
                                        .unwrap_or(Value::text("")));
                                }
                                "char_code" => {
                                    let s = from_val.as_text().unwrap_or("");
                                    return Ok(Value::number(
                                        s.chars().next().map(|c| c as u32 as f64).unwrap_or(0.0),
                                    ));
                                }
                                "from_char_code" => {
                                    let n = from_val.as_number().unwrap_or(0.0) as u32;
                                    return Ok(char::from_u32(n)
                                        .map(|c| Value::text(c.to_string()))
                                        .unwrap_or(Value::text("")));
                                }
                                "find" => {
                                    let s = from_val.as_text().unwrap_or("");
                                    let search = arguments
                                        .iter()
                                        .find(|a| a.node.name.as_str() == "search")
                                        .and_then(|a| a.node.value.as_ref())
                                        .and_then(|v| {
                                            self.eval_static_with_scope(v, local_scope).ok()
                                        })
                                        .and_then(|v| v.as_text().map(|s| s.to_string()))
                                        .unwrap_or_default();
                                    return Ok(Value::number(
                                        s.find(&search).map(|i| i as f64).unwrap_or(-1.0),
                                    ));
                                }
                                "find_closing" => {
                                    let s = from_val.as_text().unwrap_or("");
                                    let start = arguments
                                        .iter()
                                        .find(|a| a.node.name.as_str() == "start")
                                        .and_then(|a| a.node.value.as_ref())
                                        .and_then(|v| {
                                            self.eval_static_with_scope(v, local_scope).ok()
                                        })
                                        .and_then(|v| v.as_number())
                                        .unwrap_or(0.0)
                                        as usize;
                                    let open = arguments
                                        .iter()
                                        .find(|a| a.node.name.as_str() == "open")
                                        .and_then(|a| a.node.value.as_ref())
                                        .and_then(|v| {
                                            self.eval_static_with_scope(v, local_scope).ok()
                                        })
                                        .and_then(|v| v.as_text().map(|s| s.to_string()))
                                        .and_then(|s| s.chars().next())
                                        .unwrap_or('(');
                                    let close = arguments
                                        .iter()
                                        .find(|a| a.node.name.as_str() == "close")
                                        .and_then(|a| a.node.value.as_ref())
                                        .and_then(|v| {
                                            self.eval_static_with_scope(v, local_scope).ok()
                                        })
                                        .and_then(|v| v.as_text().map(|s| s.to_string()))
                                        .and_then(|s| s.chars().next())
                                        .unwrap_or(')');
                                    let mut depth = 1i64;
                                    let chars: Vec<char> = s.chars().collect();
                                    for i in start..chars.len() {
                                        if chars[i] == open {
                                            depth += 1;
                                        } else if chars[i] == close {
                                            depth -= 1;
                                            if depth == 0 {
                                                return Ok(Value::number(i as f64));
                                            }
                                        }
                                    }
                                    return Ok(Value::number(-1.0));
                                }
                                "substring" => {
                                    let s = from_val.as_text().unwrap_or("");
                                    let start = arguments
                                        .iter()
                                        .find(|a| a.node.name.as_str() == "start")
                                        .and_then(|a| a.node.value.as_ref())
                                        .and_then(|v| {
                                            self.eval_static_with_scope(v, local_scope).ok()
                                        })
                                        .and_then(|v| v.as_number())
                                        .unwrap_or(0.0)
                                        as usize;
                                    let length = arguments
                                        .iter()
                                        .find(|a| a.node.name.as_str() == "length")
                                        .and_then(|a| a.node.value.as_ref())
                                        .and_then(|v| {
                                            self.eval_static_with_scope(v, local_scope).ok()
                                        })
                                        .and_then(|v| v.as_number());
                                    let chars: Vec<char> = s.chars().collect();
                                    let end =
                                        length.map(|l| start + l as usize).unwrap_or(chars.len());
                                    let result: String = chars
                                        [start.min(chars.len())..end.min(chars.len())]
                                        .iter()
                                        .collect();
                                    return Ok(Value::text(result));
                                }
                                _ => {}
                            }
                        }
                        // Built-in Math/ functions
                        if *module == "Math" {
                            let n = from_val.as_number().unwrap_or(0.0);
                            match *fn_name {
                                "round" => return Ok(Value::number(n.round())),
                                "min" => {
                                    let b = arguments
                                        .iter()
                                        .find(|a| a.node.name.as_str() == "b")
                                        .and_then(|a| a.node.value.as_ref())
                                        .and_then(|v| {
                                            self.eval_static_with_scope(v, local_scope).ok()
                                        })
                                        .and_then(|v| v.as_number())
                                        .unwrap_or(f64::INFINITY);
                                    return Ok(Value::number(n.min(b)));
                                }
                                "max" => {
                                    let b = arguments
                                        .iter()
                                        .find(|a| a.node.name.as_str() == "b")
                                        .and_then(|a| a.node.value.as_ref())
                                        .and_then(|v| {
                                            self.eval_static_with_scope(v, local_scope).ok()
                                        })
                                        .and_then(|v| v.as_number())
                                        .unwrap_or(f64::NEG_INFINITY);
                                    return Ok(Value::number(n.max(b)));
                                }
                                "modulo" => {
                                    let divisor = arguments
                                        .iter()
                                        .find(|a| a.node.name.as_str() == "divisor")
                                        .and_then(|a| a.node.value.as_ref())
                                        .and_then(|v| {
                                            self.eval_static_with_scope(v, local_scope).ok()
                                        })
                                        .and_then(|v| v.as_number())
                                        .unwrap_or(1.0);
                                    return Ok(Value::number(n % divisor));
                                }
                                _ => {}
                            }
                        }
                        // Built-in List/ functions
                        if *module == "List" {
                            match *fn_name {
                                "sum" => {
                                    let mut sum = 0.0;
                                    if let Value::Tagged { tag, fields } = &from_val {
                                        if tag.as_ref() == LIST_TAG {
                                            for v in fields.values() {
                                                sum += v.as_number().unwrap_or(0.0);
                                            }
                                        }
                                    }
                                    return Ok(Value::number(sum));
                                }
                                "product" => {
                                    let mut prod = 1.0;
                                    if let Value::Tagged { tag, fields } = &from_val {
                                        if tag.as_ref() == LIST_TAG {
                                            for v in fields.values() {
                                                prod *= v.as_number().unwrap_or(1.0);
                                            }
                                        }
                                    }
                                    return Ok(Value::number(prod));
                                }
                                "last" => {
                                    if let Value::Tagged { tag, fields } = &from_val {
                                        if tag.as_ref() == LIST_TAG {
                                            if let Some(last) = fields.values().last() {
                                                return Ok(last.clone());
                                            }
                                        }
                                    }
                                    return Ok(Value::Unit);
                                }
                                "get" => {
                                    let index = arguments
                                        .iter()
                                        .find(|a| a.node.name.as_str() == "index")
                                        .and_then(|a| a.node.value.as_ref())
                                        .and_then(|v| {
                                            self.eval_static_with_scope(v, local_scope).ok()
                                        })
                                        .and_then(|v| v.as_number())
                                        .unwrap_or(1.0)
                                        as usize;
                                    // 1-based indexing
                                    if let Value::Tagged { tag, fields } = &from_val {
                                        if tag.as_ref() == LIST_TAG {
                                            if let Some(val) =
                                                fields.values().nth(index.saturating_sub(1))
                                            {
                                                return Ok(val.clone());
                                            }
                                        }
                                    }
                                    return Ok(Value::Unit);
                                }
                                _ => {}
                            }
                        }
                        // User-defined piped function: `value |> Theme/material()`
                        let qualified = format!("{}/{}", module, fn_name);
                        if let Some((qualified_name, params, body)) = self.find_function(&qualified)
                        {
                            let scoped = self.with_module_context(&qualified_name);
                            let mut fn_scope = local_scope.clone();
                            if let Some(first_param) = params.first() {
                                fn_scope.insert(first_param.clone(), from_val.clone());
                            }
                            for arg in arguments {
                                let arg_name = arg.node.name.as_str();
                                if let Some(ref val_expr) = arg.node.value {
                                    let val = self.eval_static_with_scope(val_expr, local_scope)?;
                                    fn_scope.insert(arg_name.to_string(), val);
                                }
                            }
                            return scoped.eval_static_with_scope(&body, &fn_scope);
                        }
                        Err(format!("Unknown piped module function: {}", qualified))
                    }
                    _ => self.eval_function_call_static(path, arguments, local_scope),
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
            Expression::While { arms } => self.eval_while_static(&from_val, arms, local_scope),
            Expression::When { arms } => self.eval_when_static(&from_val, arms, local_scope),
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
                                    val = val
                                        .get_field(part.as_str())
                                        .cloned()
                                        .unwrap_or(Value::Unit);
                                }
                                val.get_field(LINK_PATH_FIELD)
                                    .and_then(|v| v.as_text())
                                    .map(|s| s.to_string())
                            } else {
                                None
                            }
                        } else {
                            None
                        };
                        resolved.unwrap_or_else(|| {
                            parts
                                .iter()
                                .map(|p| p.as_str())
                                .collect::<Vec<_>>()
                                .join(".")
                        })
                    }
                    Alias::WithPassed { extra_parts } => {
                        // LINK { PASSED.store.elements.item_input }
                        // Resolve PASSED to get the actual link path
                        let passed = local_scope.get(PASSED_VAR);
                        if let Some(passed_val) = passed {
                            let mut val = passed_val.clone();
                            for part in extra_parts {
                                val = val.get_field(part.as_str()).cloned().unwrap_or(Value::Unit);
                            }
                            // The resolved value might be an object with __link_path__
                            val.get_field(LINK_PATH_FIELD)
                                .and_then(|v| v.as_text())
                                .map(|s| s.to_string())
                                .or_else(|| val.as_text().map(|s| s.to_string()))
                                .unwrap_or_else(|| {
                                    // Build path from extra_parts
                                    extra_parts
                                        .iter()
                                        .map(|p| p.as_str())
                                        .collect::<Vec<_>>()
                                        .join(".")
                                })
                        } else {
                            extra_parts
                                .iter()
                                .map(|p| p.as_str())
                                .collect::<Vec<_>>()
                                .join(".")
                        }
                    }
                };
                let press_path = format!("{}.event.press", link_path);
                if let Value::Tagged { tag, fields } = from_val {
                    let mut new_fields = (*fields).clone();
                    new_fields.insert(Arc::from("press_link"), Value::text(&*press_path));
                    new_fields.insert(Arc::from(LINK_PATH_FIELD), Value::text(link_path.as_str()));
                    Ok(Value::Tagged {
                        tag,
                        fields: Arc::new(new_fields),
                    })
                } else {
                    Ok(from_val)
                }
            }
            _ => self.eval_static_with_scope(to, local_scope),
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
                                    tag: Arc::from(LIST_TAG),
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
                        return val
                            .as_number()
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
        Err(format!(
            "No WHILE arm matched value: {}",
            input.to_display_string()
        ))
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
                if let Value::Tagged {
                    tag: val_tag,
                    fields: val_fields,
                } = value
                {
                    if tag.as_str() == val_tag.as_ref() {
                        let mut bindings = IndexMap::new();
                        for var in variables {
                            let field_name = var.name.as_str();
                            if let Some(field_val) = val_fields.get(field_name) {
                                if let Some(sub_pat) = &var.value {
                                    if let Some(sub_bindings) =
                                        self.match_pattern(field_val, sub_pat)
                                    {
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
            Pattern::WildCard => Some(IndexMap::new()),
            Pattern::ValueComparison { .. } => {
                // ValueComparison not yet supported in DD engine — no match
                None
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
                Ok(if a == b {
                    Value::tag("True")
                } else {
                    Value::tag("False")
                })
            }
            Comparator::NotEqual {
                operand_a,
                operand_b,
            } => {
                let a = self.eval_static_with_scope(operand_a, local_scope)?;
                let b = self.eval_static_with_scope(operand_b, local_scope)?;
                Ok(if a != b {
                    Value::tag("True")
                } else {
                    Value::tag("False")
                })
            }
            Comparator::Greater {
                operand_a,
                operand_b,
            } => {
                let a = self.eval_static_with_scope(operand_a, local_scope)?;
                let b = self.eval_static_with_scope(operand_b, local_scope)?;
                Ok(if a > b {
                    Value::tag("True")
                } else {
                    Value::tag("False")
                })
            }
            Comparator::GreaterOrEqual {
                operand_a,
                operand_b,
            } => {
                let a = self.eval_static_with_scope(operand_a, local_scope)?;
                let b = self.eval_static_with_scope(operand_b, local_scope)?;
                Ok(if a >= b {
                    Value::tag("True")
                } else {
                    Value::tag("False")
                })
            }
            Comparator::Less {
                operand_a,
                operand_b,
            } => {
                let a = self.eval_static_with_scope(operand_a, local_scope)?;
                let b = self.eval_static_with_scope(operand_b, local_scope)?;
                Ok(if a < b {
                    Value::tag("True")
                } else {
                    Value::tag("False")
                })
            }
            Comparator::LessOrEqual {
                operand_a,
                operand_b,
            } => {
                let a = self.eval_static_with_scope(operand_a, local_scope)?;
                let b = self.eval_static_with_scope(operand_b, local_scope)?;
                Ok(if a <= b {
                    Value::tag("True")
                } else {
                    Value::tag("False")
                })
            }
        }
    }

    // -----------------------------------------------------------------------
    // Reactive compilation → DataflowGraph
    // -----------------------------------------------------------------------

    fn compile_to_graph(
        &self,
        doc_expr: &Spanned<Expression>,
        render_surface: RenderSurface,
        storage_key: Option<&str>,
        persisted_holds: &std::collections::HashMap<String, Value>,
    ) -> Result<DataflowGraph, String> {
        // Try the real DD compilation path first.
        // If it fails (e.g., unsupported pattern), fall back to the general path.
        let mut ctx = GraphBuilder::new(self, storage_key, persisted_holds);
        ctx.compile_program(doc_expr, render_surface)
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
    /// Enclosing `BLOCK` bindings that must stay available when keyed retain/map
    /// is extracted into DD operators.
    scope_bindings: Vec<(String, Spanned<Expression>)>,
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
    /// Initial PASSED context captured from the document expression's PASS argument.
    /// Used by the display pipeline closure so Theme functions can resolve
    /// colors/styles using the program's initial theme configuration.
    initial_passed: Value,
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
            initial_passed: Value::Unit,
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
        // Deduplicate: reuse existing input if same kind + link_path
        if let Some(ref path) = link_path {
            for existing in &self.inputs {
                if existing.kind == kind && existing.link_path.as_ref() == Some(path) {
                    return existing.id;
                }
            }
        }
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
        render_surface: RenderSurface,
    ) -> Result<DataflowGraph, String> {
        let trace_cells = self
            .compiler
            .get_var_expr("document")
            .map(|expr| {
                expr.span.start == doc_expr.span.start && expr.span.end == doc_expr.span.end
            })
            .unwrap_or(false);
        // Pass 1: Find all reactive variables and compile them
        let mut pending: Vec<(String, Spanned<Expression>)> = self
            .compiler
            .variables
            .iter()
            .cloned()
            .filter(|(name, expr)| {
                name != "document" && name != "scene" && self.compiler.is_reactive(expr)
            })
            .collect();

        if trace_cells {
            std::println!(
                "[cells-dd] compile_program pending reactive vars count={}",
                pending.len()
            );
        }

        while !pending.is_empty() {
            let mut compiled_any = false;
            let mut deferred = Vec::new();
            let mut last_deferred_error: Option<String> = None;

            if trace_cells {
                std::println!(
                    "[cells-dd] compile_program pass pending_count={}",
                    pending.len()
                );
            }

            for (name, expr) in pending {
                if trace_cells {
                    std::println!("[cells-dd] compile reactive var {name}");
                }
                if self.reactive_vars.contains_key(&name) {
                    compiled_any = true;
                    continue;
                }

                // Derive scope prefix from dotted variable names
                // e.g., "store.nav_action" → scope_prefix = "store"
                if let Some(dot_pos) = name.rfind('.') {
                    self.scope_prefix = Some(name[..dot_pos].to_string());
                } else {
                    self.scope_prefix = None;
                }

                match self.compile_reactive_var(&name, &expr) {
                    Ok(_) => {
                        if trace_cells {
                            std::println!("[cells-dd] compiled reactive var {name}");
                        }
                        compiled_any = true
                    }
                    Err(e) if Self::should_defer_reactive_compile_error(&e) => {
                        if trace_cells {
                            std::println!("[cells-dd] deferred reactive var {name}: {e}");
                        }
                        last_deferred_error = Some(format!("Failed compiling '{}': {}", name, e));
                        deferred.push((name, expr));
                    }
                    Err(e) => {
                        return Err(format!("Failed compiling '{}': {}", name, e));
                    }
                }
            }

            if !compiled_any {
                return Err(last_deferred_error.unwrap_or_else(|| {
                    "Failed compiling reactive variables: no progress made".to_string()
                }));
            }

            pending = deferred;
        }
        self.scope_prefix = None;

        if trace_cells {
            std::println!("[cells-dd] after reactive pass1");
        }

        // Capture initial PASSED context from the document expression's PASS argument.
        // This is needed by the keyed display pipeline so Theme functions can resolve
        // colors/styles using the program's initial configuration.
        self.initial_passed = self.extract_initial_passed(doc_expr);

        if trace_cells {
            std::println!("[cells-dd] after extract_initial_passed");
        }

        // Between Pass 1 and Pass 2: Build keyed display pipeline.
        // Scans function bodies for `PASSED.store.<keyed_name> |> List/retain(...) |> List/map(...)`
        // and compiles keyed DD operators: ListRetainReactive → ListMapWithKey.
        // The display_var points to the post-retain-post-map keyed collection for O(1) per-item diffs.
        let keyed_list_output = if let Some((list_name, keyed_var)) = self.keyed_hold_vars.first() {
            if trace_cells {
                std::println!("[cells-dd] before build_display_pipeline list_name={list_name}");
            }
            let list_name = list_name.clone();
            let keyed_var = keyed_var.clone();
            let short_name = list_name
                .strip_prefix("store.")
                .unwrap_or(&list_name)
                .to_string();
            let display_var = self
                .build_display_pipeline(&list_name, &keyed_var)
                .unwrap_or_else(|_| keyed_var.clone());
            if trace_cells {
                std::println!("[cells-dd] after build_display_pipeline");
            }
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

        if trace_cells {
            std::println!("[cells-dd] before compile_document_expr");
        }

        // Pass 2: Compile the document expression
        let doc_var = self.compile_document_expr(doc_expr)?;

        if trace_cells {
            std::println!("[cells-dd] after compile_document_expr");
        }

        Ok(DataflowGraph {
            inputs: std::mem::take(&mut self.inputs),
            collections: std::mem::take(&mut self.collections),
            document: doc_var,
            render_surface,
            storage_key: self.storage_key.clone(),
            keyed_list_output,
        })
    }

    fn should_defer_reactive_compile_error(error: &str) -> bool {
        let lowered = error.to_ascii_lowercase();
        error.contains("Reactive source '")
            || error.contains("Event source '")
            || lowered.contains("not compiled yet")
            || error.contains("List/latest() requires a keyed list source")
            || lowered.contains("reactive dep '")
    }

    fn compile_reactive_var(
        &mut self,
        name: &str,
        expr: &Spanned<Expression>,
    ) -> Result<VarId, String> {
        match &expr.node {
            // Alias references to existing reactive vars
            Expression::Alias(Alias::WithoutPassed { parts, .. }) => {
                let path = parts
                    .iter()
                    .map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
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
                // Not a reactive var — try scope-prefixed static lookup, then plain static
                match {
                    if let Some(ref prefix) = self.scope_prefix {
                        let prefixed_name = format!("{}.{}", prefix, path);
                        if let Some(prefixed_expr) = self.compiler.get_var_expr(&prefixed_name) {
                            let prefixed_expr = prefixed_expr.clone();
                            self.compiler
                                .eval_static_with_scope(&prefixed_expr, &IndexMap::new())
                        } else {
                            self.compiler.eval_static(expr)
                        }
                    } else {
                        self.compiler.eval_static(expr)
                    }
                } {
                    Ok(value) => {
                        let var = self.fresh_var(&format!("{}_literal", name));
                        self.collections
                            .insert(var.clone(), CollectionSpec::Literal(value));
                        Ok(var)
                    }
                    Err(e) => Err(format!(
                        "Alias '{}' is not a reactive var and cannot be evaluated statically: {}",
                        path, e
                    )),
                }
            }

            // Literal values (e.g., `10` as an operand in arithmetic)
            Expression::Literal(_) => match self.compiler.eval_static(expr) {
                Ok(value) => {
                    let var = self.fresh_var(&format!("{}_literal", name));
                    self.collections
                        .insert(var.clone(), CollectionSpec::Literal(value));
                    Ok(var)
                }
                Err(e) => Err(format!("Cannot evaluate literal for '{}': {}", name, e)),
            },

            Expression::Pipe { from, to } => {
                // Unwrap pipe chains to find the core reactive pattern
                self.compile_reactive_pipe(name, from, to)
            }

            // Pattern: `LATEST { ... }`
            Expression::Latest { inputs } => self.compile_latest(name, inputs),

            // Pattern: `Router/route()`
            Expression::FunctionCall { path, .. }
                if {
                    let p: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                    p.as_slice() == ["Router", "route"]
                } =>
            {
                let input_id = self.add_input(InputKind::Router, Some(ROUTER_INPUT.to_string()));
                let var = VarId::new(name);
                self.collections
                    .insert(var.clone(), CollectionSpec::Input(input_id));
                self.reactive_vars.insert(name.to_string(), var.clone());
                Ok(var)
            }

            // Pattern: `reactive_a - reactive_b` (or +, *, /)
            Expression::ArithmeticOperator(op) => self.compile_reactive_arithmetic(name, op),

            // Pattern: `reactive_a == reactive_b` (or !=, <, >, <=, >=)
            Expression::Comparator(cmp) => self.compile_reactive_comparison(name, cmp),

            // Objects with reactive fields: individual fields are compiled via
            // flattened dotted-name variables (e.g., "theme_options.name").
            // The parent Object itself doesn't need a DD collection.
            Expression::Object(_) => Ok(VarId::new(name)),

            _ => {
                // Check if this is an element with LINK bindings
                if self.compiler.expr_contains_link(expr) {
                    // Element definitions are static but contain LINK markers
                    // Don't add to reactive collections
                    Ok(VarId::new(name))
                } else {
                    Err(format!(
                        "Unsupported reactive pattern for '{}': {:?}",
                        name,
                        std::mem::discriminant(&expr.node)
                    ))
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
        let (operand_a, operand_b, op_fn): (_, _, Arc<dyn Fn(&Value, &Value) -> Value + 'static>) =
            match op {
                ArithmeticOperator::Add {
                    operand_a,
                    operand_b,
                } => (
                    operand_a,
                    operand_b,
                    Arc::new(|a: &Value, b: &Value| {
                        Value::number(a.as_number().unwrap_or(0.0) + b.as_number().unwrap_or(0.0))
                    }),
                ),
                ArithmeticOperator::Subtract {
                    operand_a,
                    operand_b,
                } => (
                    operand_a,
                    operand_b,
                    Arc::new(|a: &Value, b: &Value| {
                        Value::number(a.as_number().unwrap_or(0.0) - b.as_number().unwrap_or(0.0))
                    }),
                ),
                ArithmeticOperator::Multiply {
                    operand_a,
                    operand_b,
                } => (
                    operand_a,
                    operand_b,
                    Arc::new(|a: &Value, b: &Value| {
                        Value::number(a.as_number().unwrap_or(0.0) * b.as_number().unwrap_or(0.0))
                    }),
                ),
                ArithmeticOperator::Divide {
                    operand_a,
                    operand_b,
                } => (
                    operand_a,
                    operand_b,
                    Arc::new(|a: &Value, b: &Value| {
                        let divisor = b.as_number().unwrap_or(1.0);
                        Value::number(a.as_number().unwrap_or(0.0) / divisor)
                    }),
                ),
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
        let (operand_a, operand_b, cmp_fn): (_, _, Arc<dyn Fn(&Value, &Value) -> Value + 'static>) =
            match cmp {
                Comparator::Equal {
                    operand_a,
                    operand_b,
                } => (
                    operand_a,
                    operand_b,
                    Arc::new(|a: &Value, b: &Value| {
                        if a == b {
                            Value::tag("True")
                        } else {
                            Value::tag("False")
                        }
                    }),
                ),
                Comparator::NotEqual {
                    operand_a,
                    operand_b,
                } => (
                    operand_a,
                    operand_b,
                    Arc::new(|a: &Value, b: &Value| {
                        if a != b {
                            Value::tag("True")
                        } else {
                            Value::tag("False")
                        }
                    }),
                ),
                Comparator::Greater {
                    operand_a,
                    operand_b,
                } => (
                    operand_a,
                    operand_b,
                    Arc::new(|a: &Value, b: &Value| {
                        if a > b {
                            Value::tag("True")
                        } else {
                            Value::tag("False")
                        }
                    }),
                ),
                Comparator::GreaterOrEqual {
                    operand_a,
                    operand_b,
                } => (
                    operand_a,
                    operand_b,
                    Arc::new(|a: &Value, b: &Value| {
                        if a >= b {
                            Value::tag("True")
                        } else {
                            Value::tag("False")
                        }
                    }),
                ),
                Comparator::Less {
                    operand_a,
                    operand_b,
                } => (
                    operand_a,
                    operand_b,
                    Arc::new(|a: &Value, b: &Value| {
                        if a < b {
                            Value::tag("True")
                        } else {
                            Value::tag("False")
                        }
                    }),
                ),
                Comparator::LessOrEqual {
                    operand_a,
                    operand_b,
                } => (
                    operand_a,
                    operand_b,
                    Arc::new(|a: &Value, b: &Value| {
                        if a <= b {
                            Value::tag("True")
                        } else {
                            Value::tag("False")
                        }
                    }),
                ),
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
                                CollectionSpec::Skip {
                                    source: hold_var,
                                    count: 1,
                                },
                            );
                            self.reactive_vars.insert(name.to_string(), sum_var.clone());
                            Ok(sum_var)
                        }
                    }

                    // Pattern: `... |> Timer/interval()`
                    ["Timer", "interval"] => self.compile_timer(name, from),

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
                        self.reactive_vars
                            .insert(name.to_string(), skip_var.clone());
                        Ok(skip_var)
                    }

                    // Pattern: `... |> Document/new()`
                    ["Document", "new"] => {
                        // This is handled in compile_document_expr, not here
                        Err(format!(
                            "Document/new() should be handled at document level for '{}'",
                            name
                        ))
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
                        self.reactive_vars
                            .insert(name.to_string(), goto_var.clone());
                        Ok(goto_var)
                    }

                    // Pattern: `... |> List/clear(on: event_source)`
                    // The `from` is typically `LIST {} |> List/append(item: X)`
                    ["List", "clear"] => self.compile_list_chain(name, from, arguments),

                    // Pattern: `... |> List/remove_last(on: event_source)`
                    ["List", "remove_last"] => {
                        let mut ops = Vec::new();
                        ops.push(ListChainOp::RemoveLast(arguments));
                        let initial_list_expr = self.collect_list_chain_ops(from, &mut ops)?;
                        self.build_unified_list_holdstate(name, initial_list_expr, &ops)
                    }

                    // Pattern: `LIST {} |> List/append(item: X)` (without clear)
                    ["List", "append"] => self.compile_list_append_only(name, from, arguments),

                    // Pattern: `source |> List/count()`
                    ["List", "count"] => self.compile_list_count(name, from),

                    // Pattern: `source |> List/latest()`
                    ["List", "latest"] => self.compile_list_latest(name, from),

                    // Pattern: `source |> List/every(item, if: predicate)`
                    ["List", "every"] => self.compile_list_every(name, from, arguments),

                    // Pattern: `source |> List/any(item, if: predicate)`
                    ["List", "any"] => self.compile_list_any(name, from, arguments),

                    // Pattern: `source |> List/is_not_empty()`
                    ["List", "is_not_empty"] => self.compile_list_emptiness_check(name, from, true),

                    // Pattern: `source |> List/is_empty()`
                    ["List", "is_empty"] => self.compile_list_emptiness_check(name, from, false),

                    // Pattern: `source |> List/retain(item, if: predicate)`
                    ["List", "retain"] => self.compile_list_retain(name, from, arguments),

                    // Pattern: `source |> List/map(item, new: transform)`
                    ["List", "map"] => self.compile_list_map(name, from, arguments),

                    // Pattern: `source |> List/remove(on: event)`
                    ["List", "remove"] => self.compile_list_remove(name, from, arguments),

                    // Pattern: `initial |> Bool/toggle(when: event_source)`
                    ["Bool", "toggle"] => self.compile_bool_toggle(name, from, arguments),

                    _ => {
                        // Try user-defined function: `source |> my_function()`
                        // or module-qualified: `source |> Theme/material()`
                        // Compiled as a Map that evaluates the function body per input.
                        let qualified_fn_name = if path.len() == 1 {
                            let name = path[0].as_str().to_string();
                            // Try exact match, then intra-module resolution
                            if self.compiler.find_function(&name).is_some() {
                                Some(name)
                            } else {
                                None
                            }
                        } else if path.len() == 2 {
                            let qualified = format!("{}/{}", path[0].as_str(), path[1].as_str());
                            if self.compiler.find_function(&qualified).is_some() {
                                Some(qualified)
                            } else {
                                None
                            }
                        } else {
                            None
                        };

                        if let Some(fn_name) = qualified_fn_name {
                            let source_var = self.resolve_reactive_source(from)?;
                            let mut compiler = self.compiler.clone();
                            // Set module context for intra-module resolution in the closure
                            compiler.current_module =
                                compiler.function_modules.get(&fn_name).cloned();
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
                                        let func = compiler.find_function(&fn_name);
                                        if let Some((_, params, body)) = func {
                                            let mut scope = IndexMap::new();
                                            // Bind the piped value to the first parameter
                                            if let Some(first_param) = params.first() {
                                                scope.insert(first_param.clone(), input.clone());
                                            }
                                            // Bind explicit arguments
                                            for (arg_name, arg_val) in &args_clone {
                                                if let Some(val_expr) = arg_val {
                                                    if let Ok(v) = compiler
                                                        .eval_static_with_scope(val_expr, &scope)
                                                    {
                                                        scope.insert(arg_name.clone(), v);
                                                    }
                                                }
                                            }
                                            // Bind by parameter position
                                            for (i, p) in params.iter().enumerate().skip(1) {
                                                if i - 1 < args_clone.len() {
                                                    if let Some(val_expr) = &args_clone[i - 1].1 {
                                                        if let Ok(v) = compiler
                                                            .eval_static_with_scope(
                                                                val_expr, &scope,
                                                            )
                                                        {
                                                            scope.insert(p.clone(), v);
                                                        }
                                                    }
                                                }
                                            }
                                            compiler.eval_static_tolerant(&body, &scope)
                                        } else {
                                            Value::Unit
                                        }
                                    }),
                                },
                            );
                            self.reactive_vars.insert(name.to_string(), map_var.clone());
                            return Ok(map_var);
                        }

                        // Fallback: try built-in function as a Map
                        // (e.g., Bool/not, Text/trim, Text/is_not_empty, List/is_empty)
                        let fn_path: Vec<String> =
                            path.iter().map(|s| s.as_str().to_string()).collect();
                        let args_for_builtin: Vec<(String, Option<Spanned<Expression>>)> =
                            arguments
                                .iter()
                                .map(|a| (a.node.name.as_str().to_string(), a.node.value.clone()))
                                .collect();
                        if self.compiler.is_builtin_piped_fn(&fn_path) {
                            let source_var = self.resolve_reactive_source(from)?;

                            // Check if any argument references a reactive variable.
                            // If so, use Join to combine source and arg, then Map.
                            let reactive_arg = self.find_first_reactive_argument(&args_for_builtin);

                            if let Some((_arg_name, arg_var)) = reactive_arg {
                                // Reactive argument: Join source + arg, then Map
                                let join_var = self.fresh_var(&format!("{}_builtin_join", name));
                                self.collections.insert(
                                    join_var.clone(),
                                    CollectionSpec::Join {
                                        left: source_var,
                                        right: arg_var,
                                        combine: Arc::new(|a: &Value, b: &Value| {
                                            Value::object([
                                                ("__src", a.clone()),
                                                ("__arg", b.clone()),
                                            ])
                                        }),
                                    },
                                );
                                let fn_path_for_closure = fn_path.clone();
                                let map_var = VarId::new(name);
                                self.collections.insert(
                                    map_var.clone(),
                                    CollectionSpec::Map {
                                        source: join_var,
                                        f: Arc::new(move |joined: &Value| {
                                            let src = joined
                                                .get_field("__src")
                                                .cloned()
                                                .unwrap_or(Value::Unit);
                                            let arg = joined
                                                .get_field("__arg")
                                                .cloned()
                                                .unwrap_or(Value::Unit);
                                            let strs: Vec<&str> = fn_path_for_closure
                                                .iter()
                                                .map(|s| s.as_str())
                                                .collect();
                                            match strs.as_slice() {
                                                ["Math", "min"] => Value::number(
                                                    src.as_number()
                                                        .unwrap_or(0.0)
                                                        .min(arg.as_number().unwrap_or(0.0)),
                                                ),
                                                ["Math", "max"] => Value::number(
                                                    src.as_number()
                                                        .unwrap_or(0.0)
                                                        .max(arg.as_number().unwrap_or(0.0)),
                                                ),
                                                ["Math", "modulo"] => Value::number(
                                                    src.as_number().unwrap_or(0.0)
                                                        % arg.as_number().unwrap_or(1.0),
                                                ),
                                                ["Text", "starts_with"] => {
                                                    let s = src.as_text().unwrap_or("");
                                                    let prefix = arg.as_text().unwrap_or("");
                                                    if s.starts_with(prefix) {
                                                        Value::tag("True")
                                                    } else {
                                                        Value::tag("False")
                                                    }
                                                }
                                                _ => src.clone(),
                                            }
                                        }),
                                    },
                                );
                                self.reactive_vars.insert(name.to_string(), map_var.clone());
                                return Ok(map_var);
                            }

                            // All args are static: use simple Map
                            let compiler = self.compiler.clone();
                            let map_var = VarId::new(name);
                            self.collections.insert(
                                map_var.clone(),
                                CollectionSpec::Map {
                                    source: source_var,
                                    f: Arc::new(move |input: &Value| {
                                        compiler
                                            .eval_builtin_piped(input, &fn_path, &args_for_builtin)
                                            .unwrap_or(Value::Unit)
                                    }),
                                },
                            );
                            self.reactive_vars.insert(name.to_string(), map_var.clone());
                            return Ok(map_var);
                        }

                        Err(format!(
                            "Unsupported function in reactive pipe for '{}': {}",
                            name,
                            path_strs.join("/")
                        ))
                    }
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
                self.reactive_vars
                    .insert(name.to_string(), when_var.clone());
                Ok(when_var)
            }

            // Pattern: `event_source |> THEN { body }`
            Expression::Then { body } => {
                let (source_var, _) = self.compile_event_source(from)?;
                let event_source_name = Self::extract_hold_event_source_name(from);
                let mut reactive_deps = self.find_reactive_deps_in_expr(body);
                self.collect_ensured_text_reactive_deps(body, &mut reactive_deps);
                Self::retain_non_event_reactive_deps(
                    &mut reactive_deps,
                    event_source_name.as_ref(),
                );
                let then_var = VarId::new(name);
                let uses_event_alias = event_source_name
                    .as_ref()
                    .map(|name| Self::expr_references_name(body, name))
                    .unwrap_or(false);
                if reactive_deps.is_empty() && !uses_event_alias {
                    let transform = self.build_then_transform(body);
                    self.collections.insert(
                        then_var.clone(),
                        CollectionSpec::Then {
                            source: source_var,
                            body: transform,
                        },
                    );
                } else if reactive_deps.is_empty() {
                    let transform = self.build_then_dep_transform(
                        body,
                        &[],
                        self.scope_prefix.clone(),
                        event_source_name,
                    );
                    self.collections.insert(
                        then_var.clone(),
                        CollectionSpec::Map {
                            source: source_var,
                            f: transform,
                        },
                    );
                } else {
                    let joined = self.join_event_with_reactive_deps(source_var, &reactive_deps)?;
                    let transform = self.build_then_dep_transform(
                        body,
                        &reactive_deps,
                        self.scope_prefix.clone(),
                        event_source_name,
                    );
                    self.collections.insert(
                        then_var.clone(),
                        CollectionSpec::Map {
                            source: joined,
                            f: transform,
                        },
                    );
                }
                self.reactive_vars
                    .insert(name.to_string(), then_var.clone());
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
                self.reactive_vars
                    .insert(name.to_string(), while_var.clone());
                Ok(while_var)
            }

            _ => Err(format!(
                "Unsupported reactive pipe target for '{}': {:?}",
                name,
                std::mem::discriminant(&to.node)
            )),
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
        let input_id =
            self.add_input_with_timer(InputKind::Timer, Some(name.to_string()), Some(seconds));
        let timer_var = VarId::new(name);
        self.collections
            .insert(timer_var.clone(), CollectionSpec::Input(input_id));
        self.reactive_vars
            .insert(name.to_string(), timer_var.clone());
        Ok(timer_var)
    }

    fn extract_duration_seconds(&self, expr: &Spanned<Expression>) -> Result<f64, String> {
        match &expr.node {
            Expression::TaggedObject { tag, object } => {
                if tag.as_str() == "Duration" {
                    for var in &object.variables {
                        if var.node.name.as_str() == "seconds" {
                            return self.compiler.eval_static(&var.node.value).and_then(|v| {
                                v.as_number()
                                    .ok_or_else(|| "Duration seconds must be a number".to_string())
                            });
                        }
                        if var.node.name.as_str() == "milliseconds" {
                            return self.compiler.eval_static(&var.node.value).and_then(|v| {
                                v.as_number().map(|ms| ms / 1000.0).ok_or_else(|| {
                                    "Duration milliseconds must be a number".to_string()
                                })
                            });
                        }
                    }
                    Err("Duration missing 'seconds' or 'milliseconds' field".to_string())
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
        let initial_value = self
            .compiler
            .eval_static(initial_expr)
            .map_err(|e| format!("Cannot evaluate initial value for '{}': {}", name, e))?;

        // Check for persisted value to override initial
        let effective_initial = self
            .persisted_holds
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
        let (events_var, link_path) = self.compile_hold_body_events(name, state_param, body)?;

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

        self.reactive_vars
            .insert(name.to_string(), hold_var.clone());

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
        state_param: &str,
        body: &Spanned<Expression>,
    ) -> Result<(VarId, Option<String>), String> {
        // Body is typically: `event_source |> THEN { transform }`
        match &body.node {
            Expression::Pipe { from, to } => {
                match &to.node {
                    Expression::Then { body: then_body } => {
                        // `from` is the event source (e.g., increment_button.event.press)
                        let (source_var, link_path) = self.compile_event_source(from)?;
                        let event_source_name = Self::extract_hold_event_source_name(from);
                        let depends_on_state = Self::expr_references_name(then_body, state_param);

                        // Create a THEN collection
                        let then_var = self.fresh_var(&format!("{}_then", hold_name));
                        let mut reactive_deps = self.find_reactive_deps_in_expr(then_body);
                        self.collect_ensured_text_reactive_deps(then_body, &mut reactive_deps);
                        reactive_deps.retain(|dep| dep != state_param && dep != hold_name);
                        Self::retain_non_event_reactive_deps(
                            &mut reactive_deps,
                            event_source_name.as_ref(),
                        );
                        if depends_on_state {
                            return Ok((source_var, link_path));
                        }
                        let uses_event_alias = event_source_name
                            .as_ref()
                            .map(|name| Self::expr_references_name(then_body, name))
                            .unwrap_or(false);
                        if reactive_deps.is_empty() && !uses_event_alias {
                            let transform = self.build_then_transform(then_body);
                            self.collections.insert(
                                then_var.clone(),
                                CollectionSpec::Then {
                                    source: source_var,
                                    body: transform,
                                },
                            );
                        } else if reactive_deps.is_empty() {
                            let transform = self.build_then_dep_transform(
                                then_body,
                                &[],
                                self.scope_prefix.clone(),
                                event_source_name,
                            );
                            self.collections.insert(
                                then_var.clone(),
                                CollectionSpec::Map {
                                    source: source_var,
                                    f: transform,
                                },
                            );
                        } else {
                            let joined =
                                self.join_event_with_reactive_deps(source_var, &reactive_deps)?;
                            let transform = self.build_then_dep_transform(
                                then_body,
                                &reactive_deps,
                                self.scope_prefix.clone(),
                                event_source_name,
                            );
                            self.collections.insert(
                                then_var.clone(),
                                CollectionSpec::Map {
                                    source: joined,
                                    f: transform,
                                },
                            );
                        }

                        Ok((then_var, link_path))
                    }
                    _ => {
                        // Direct reactive stream without THEN, for example
                        // `people |> List/map(...) |> List/latest()` inside a HOLD body.
                        let inline_name = format!("{}__pipe_event", hold_name);
                        let var_id = self.compile_reactive_var(&inline_name, body)?;
                        Ok((var_id, None))
                    }
                }
            }
            // LATEST { stream1 |> THEN { val1 }, stream2 |> THEN { val2 }, ... }
            // Merge multiple event sources into one.
            Expression::Latest { inputs } => {
                let mut event_vars = Vec::new();
                for (i, input) in inputs.iter().enumerate() {
                    let sub_name = format!("{}__latest_{}", hold_name, i);
                    let (var_id, _) =
                        self.compile_hold_body_events(&sub_name, state_param, input)?;
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
                self.collections
                    .insert(concat_var.clone(), CollectionSpec::Concat(event_vars));
                // HoldLatest to keep only the most recent event
                let latest_var = self.fresh_var(&format!("{}_events_latest", hold_name));
                self.collections.insert(
                    latest_var.clone(),
                    CollectionSpec::HoldLatest(vec![concat_var]),
                );
                Ok((latest_var, None))
            }

            // Bare alias referencing an event path: event value replaces state.
            // e.g., `Text/empty() |> HOLD state { elements.input.event.change.text }`
            Expression::Alias(_) => self.compile_event_source(body),

            _ => Err(format!("Unsupported HOLD body pattern for '{}'", hold_name)),
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
                    if let Some(var_expr) = self.compiler.get_var_expr(var_name) {
                        if self.compiler.is_reactive(var_expr) {
                            return Ok((VarId::new(var_name), None));
                        }
                    }
                }

                // Build the effective path (with scope prefix if needed)
                // Avoid double-prepend if the path already starts with the prefix.
                // Also skip prepend when the first segment is already a known top-level
                // scope (e.g., `store.elements...` referenced from `theme_options.mode`).
                let effective_path = if let Some(ref prefix) = self.scope_prefix {
                    if path_str.starts_with(&format!("{}.", prefix)) {
                        path_str.clone()
                    } else {
                        let first_seg_is_known = self.compiler.variables.iter().any(|(n, _)| {
                            n == var_name || n.starts_with(&format!("{}.", var_name))
                        });
                        if first_seg_is_known {
                            // Path already starts with a known scope — use as-is
                            path_str.clone()
                        } else {
                            format!("{}.{}", prefix, path_str)
                        }
                    }
                } else {
                    path_str.clone()
                };

                // Check if the path contains an event reference
                if effective_path.contains(".event.") {
                    let (kind, link_path) = Self::detect_event_kind_and_path(&effective_path);
                    let input_id = self.add_input(kind, Some(link_path.clone()));
                    let input_var = self.fresh_var("link_input");
                    self.collections
                        .insert(input_var.clone(), CollectionSpec::Input(input_id));
                    return Ok((input_var, Some(link_path)));
                }

                // Check if this references a variable with a LINK element
                if let Some(var_expr) = self.compiler.get_var_expr(var_name) {
                    if self.compiler.expr_contains_link(var_expr) {
                        let input_id =
                            self.add_input(InputKind::LinkPress, Some(effective_path.clone()));
                        let input_var = self.fresh_var("link_input");
                        self.collections
                            .insert(input_var.clone(), CollectionSpec::Input(input_id));
                        return Ok((input_var, Some(effective_path)));
                    }
                }

                // Try with scope prefix for nested object references
                if let Some(ref prefix) = self.scope_prefix {
                    let prefixed_var_name = format!("{}.{}", prefix, var_name);
                    if let Some(var_expr) = self.compiler.get_var_expr(&prefixed_var_name) {
                        if self.compiler.expr_contains_link(var_expr) {
                            let input_id =
                                self.add_input(InputKind::LinkPress, Some(effective_path.clone()));
                            let input_var = self.fresh_var("link_input");
                            self.collections
                                .insert(input_var.clone(), CollectionSpec::Input(input_id));
                            return Ok((input_var, Some(effective_path)));
                        }
                    }
                }

                Err(format!(
                    "Event source '{}' not found as reactive var or LINK element",
                    path_str
                ))
            }
            // Inline reactive pipe as event source (e.g., Duration |> Timer/interval())
            Expression::Pipe { .. } => {
                let inline_name = format!("__inline_event_{}", self.next_anon_id);
                let var_id = self.compile_reactive_var(&inline_name, expr)?;
                Ok((var_id, None))
            }
            _ => Err(format!(
                "Unsupported event source expression: {:?}",
                std::mem::discriminant(&expr.node)
            )),
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
        self.collections
            .insert(latest_var.clone(), CollectionSpec::HoldLatest(source_vars));

        self.reactive_vars
            .insert(name.to_string(), latest_var.clone());
        Ok(latest_var)
    }

    fn compile_latest_input(
        &mut self,
        parent_name: &str,
        index: usize,
        expr: &Spanned<Expression>,
    ) -> Result<VarId, String> {
        // Direct literals (tags like None/True/False, numbers, strings)
        if let Expression::Literal(lit) = &expr.node {
            let val = Compiler::eval_literal(lit);
            let var = self.fresh_var(&format!("{}_lit{}", parent_name, index));
            self.collections
                .insert(var.clone(), CollectionSpec::Literal(val));
            return Ok(var);
        }

        // Try static evaluation only for expressions that are not reactive.
        // Running eval_static on a reactive pipe can recurse through forward
        // references (for example CRUD's `people |> List/map(...) |> List/latest()`
        // inside `selected_id`) and hang the compiler.
        if !self.compiler.is_reactive(expr) {
            if let Ok(val) = self.compiler.eval_static(expr) {
                let var = self.fresh_var(&format!("{}_const{}", parent_name, index));
                self.collections
                    .insert(var.clone(), CollectionSpec::Literal(val));
                return Ok(var);
            }
        }

        // Delegate to general reactive compilation for everything else
        // (pipes with THEN/WHEN/WHILE/FunctionCall, aliases, nested LATEST, etc.)
        let var_name = format!("{}_input{}", parent_name, index);
        self.compile_reactive_var(&var_name, expr)
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
        let effective_initial = self
            .persisted_holds
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
            self.collections
                .insert(concat_var.clone(), CollectionSpec::Concat(event_vars));
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

    fn compile_document_expr(&mut self, expr: &Spanned<Expression>) -> Result<VarId, String> {
        #[cfg(test)]
        let trace_cells = self
            .compiler
            .get_var_expr("document")
            .map(|doc| doc.span.start == expr.span.start && doc.span.end == expr.span.end)
            .unwrap_or(false);
        // Handle `reactive_var |> Document/new()` / `reactive_var |> Scene/new()` pipe patterns.
        if let Expression::Pipe { from, to } = &expr.node {
            if let Expression::FunctionCall { arguments, .. } = &to.node {
                if let Some(render_tag) = Self::render_root_tag_from_expr(to) {
                    if arguments.is_empty() {
                        // Try simple alias first (e.g., `counter |> Document/new()`)
                        if let Ok(var) = self.compile_piped_document(from, render_tag) {
                            return Ok(var);
                        }
                        // Fall through to chain compilation for pipe expressions
                        return self.compile_piped_document_chain(expr, render_tag);
                    }
                }
            }
        }

        // Standard pattern: `Document/new(root: some_element)`
        // Filter to only reactive vars with guaranteed initial values.
        // Event-only vars (LINK inputs, WHEN with SKIP) would block the Join chain.
        let reactive_deps: Vec<String> = self
            .reactive_vars
            .iter()
            .filter(|(name, var_id)| {
                // Must have an initial value at runtime
                if !self.has_initial_value(var_id) {
                    return false;
                }
                // Only include user-defined variables (from parser), not compiler-generated
                // intermediates like LATEST arm inputs (*_input0, *_input1) or internal
                // pipe stages (__pipe_source_*). System vars (__router) are kept.
                if name.starts_with("__") {
                    return true;
                }
                self.compiler.get_var_expr(name).is_some()
            })
            .map(|(name, _)| name.clone())
            .collect();

        #[cfg(test)]
        if trace_cells {
            eprintln!(
                "[cells-dd] compile_document_expr reactive_deps={}",
                reactive_deps.join(", ")
            );
        }

        if reactive_deps.is_empty() {
            // Static document
            let val = self
                .compiler
                .eval_static(expr)
                .map_err(|e| format!("Cannot evaluate document: {}", e))?;
            let doc_var = self.fresh_var("document");
            self.collections
                .insert(doc_var.clone(), CollectionSpec::Literal(val));
            return Ok(doc_var);
        }

        // For single reactive dependency (common case: counter_hold, counter)
        if reactive_deps.len() == 1 {
            let dep_name = &reactive_deps[0];
            let dep_var = self.reactive_vars.get(dep_name).unwrap().clone();

            if self.expr_contains_user_function_call(expr) {
                #[cfg(test)]
                if trace_cells {
                    eprintln!(
                        "[cells-dd] using single-dep eval closure for {dep_name} due to user function calls"
                    );
                }
                let compiler_clone = self.compiler.clone();
                let doc_expr_clone = expr.clone();
                let dep_name_clone = dep_name.clone();
                let doc_var = self.fresh_var("document");
                self.collections.insert(
                    doc_var.clone(),
                    CollectionSpec::Map {
                        source: dep_var,
                        f: Arc::new(move |reactive_value: &Value| {
                            let mut scope = IndexMap::new();
                            scope.insert(
                                dep_name_clone.clone(),
                                decorate_reactive_scope_value(reactive_value, &dep_name_clone),
                            );
                            compiler_clone
                                .eval_static_with_scope(&doc_expr_clone, &scope)
                                .unwrap_or(Value::Unit)
                        }),
                    },
                );
                return Ok(doc_var);
            }

            // Build a document template closure.
            // Falls back to closure-based evaluation when the template can't
            // interpolate the reactive value (e.g., used via PASSED inside
            // a user function call like root_element).
            match self.build_document_closure(dep_name, expr) {
                Ok(doc_closure) => {
                    #[cfg(test)]
                    if trace_cells {
                        eprintln!("[cells-dd] build_document_closure succeeded for {dep_name}");
                    }
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
                Err(_) => {
                    #[cfg(test)]
                    if trace_cells {
                        eprintln!("[cells-dd] build_document_closure fell back for {dep_name}");
                    }
                    return self.compile_multi_dep_document(expr, &reactive_deps);
                }
            }
        }

        // Multiple reactive dependencies — try find root and derive others first
        match self.find_root_and_derived(&reactive_deps) {
            Ok((root_dep, derived_deps)) => {
                let root_var = self.reactive_vars.get(&root_dep).unwrap().clone();

                // Build derived variable computations (closures that derive from root value)
                let mut derived_fns: Vec<(String, Arc<dyn Fn(&Value) -> Value + 'static>)> =
                    Vec::new();
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

    /// Compile `reactive_var |> Document/new()` or `reactive_var |> Scene/new()`
    /// where the source is a simple alias.
    fn compile_piped_document(
        &mut self,
        from: &Spanned<Expression>,
        render_tag: &'static str,
    ) -> Result<VarId, String> {
        // Check if `from` references a reactive variable
        if let Expression::Alias(Alias::WithoutPassed { parts, .. }) = &from.node {
            if parts.len() == 1 {
                let name = parts[0].as_str();
                if let Some(source_var) = self.reactive_vars.get(name).cloned() {
                    // Map the reactive value into the requested render-root wrapper.
                    let doc_var = self.fresh_var("document");
                    self.collections.insert(
                        doc_var.clone(),
                        CollectionSpec::Map {
                            source: source_var,
                            f: Arc::new(move |v: &Value| {
                                Value::tagged(render_tag, [("root", v.clone())])
                            }),
                        },
                    );
                    return Ok(doc_var);
                }
            }
        }
        Err("Unsupported piped document source".to_string())
    }

    /// Compile a full reactive pipe chain as the render root.
    /// e.g., `Duration |> Timer/interval() |> THEN { 1 } |> Math/sum() |> Document/new()`
    fn compile_piped_document_chain(
        &mut self,
        expr: &Spanned<Expression>,
        render_tag: &'static str,
    ) -> Result<VarId, String> {
        // Peel off the outer render-root wrapper and compile the inner chain as a reactive var.
        if let Expression::Pipe { from, to } = &expr.node {
            if let Some(tag) = Self::render_root_tag_from_expr(to) {
                if tag == render_tag {
                    // Compile the inner chain as a reactive variable named "__doc_source".
                    let source_var = self.compile_reactive_var("__doc_source", from)?;
                    // Wrap in the requested render-root tag.
                    let doc_var = self.fresh_var("document");
                    self.collections.insert(
                        doc_var.clone(),
                        CollectionSpec::Map {
                            source: source_var,
                            f: Arc::new(move |v: &Value| {
                                Value::tagged(render_tag, [("root", v.clone())])
                            }),
                        },
                    );
                    return Ok(doc_var);
                }
            }
        }
        Err("Cannot compile piped document chain".to_string())
    }

    fn render_root_tag_from_expr(expr: &Spanned<Expression>) -> Option<&'static str> {
        let Expression::FunctionCall { path, .. } = &expr.node else {
            return None;
        };
        let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
        match path_strs.as_slice() {
            ["Document", "new"] => Some("DocumentNew"),
            ["Scene", "new"] => Some("SceneNew"),
            _ => None,
        }
    }

    fn is_reactive_pipe(&self, expr: &Spanned<Expression>) -> bool {
        match &expr.node {
            Expression::Pipe { from, to } => {
                self.is_reactive_pipe(from) || self.is_reactive_pipe(to)
            }
            Expression::FunctionCall { path, .. } => {
                let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                matches!(
                    path_strs.as_slice(),
                    ["Timer", "interval"]
                        | ["Math", "sum"]
                        | ["Stream", "skip"]
                        | ["Document", "new"]
                )
            }
            Expression::Then { .. } | Expression::Hold { .. } | Expression::Latest { .. } => true,
            Expression::TaggedObject { tag, .. } => tag.as_str() == "Duration",
            _ => false,
        }
    }

    fn expr_contains_user_function_call(&self, expr: &Spanned<Expression>) -> bool {
        match &expr.node {
            Expression::FunctionCall { path, arguments } => {
                let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                let is_user_function = match path_strs.as_slice() {
                    [name] => self.compiler.find_function(name).is_some(),
                    [module, name] => self
                        .compiler
                        .find_function(&format!("{module}/{name}"))
                        .is_some(),
                    _ => false,
                };
                is_user_function
                    || arguments.iter().any(|arg| {
                        arg.node
                            .value
                            .as_ref()
                            .map(|value| self.expr_contains_user_function_call(value))
                            .unwrap_or(false)
                    })
            }
            Expression::Pipe { from, to } => {
                self.expr_contains_user_function_call(from)
                    || self.expr_contains_user_function_call(to)
            }
            Expression::List { items } => items
                .iter()
                .any(|item| self.expr_contains_user_function_call(item)),
            Expression::Object(object) => object
                .variables
                .iter()
                .any(|var| self.expr_contains_user_function_call(&var.node.value)),
            Expression::Block { variables, output } => {
                variables
                    .iter()
                    .any(|var| self.expr_contains_user_function_call(&var.node.value))
                    || self.expr_contains_user_function_call(output)
            }
            Expression::Latest { inputs } => inputs
                .iter()
                .any(|input| self.expr_contains_user_function_call(input)),
            Expression::Then { body } => self.expr_contains_user_function_call(body),
            Expression::While { arms } => arms
                .iter()
                .any(|arm| self.expr_contains_user_function_call(&arm.body)),
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
                } => {
                    self.expr_contains_user_function_call(operand_a)
                        || self.expr_contains_user_function_call(operand_b)
                }
                ArithmeticOperator::Negate { operand } => {
                    self.expr_contains_user_function_call(operand)
                }
            },
            Expression::Comparator(cmp) => {
                use static_expression::Comparator;
                let (a, b) = match cmp {
                    Comparator::Equal {
                        operand_a,
                        operand_b,
                    }
                    | Comparator::NotEqual {
                        operand_a,
                        operand_b,
                    }
                    | Comparator::Greater {
                        operand_a,
                        operand_b,
                    }
                    | Comparator::GreaterOrEqual {
                        operand_a,
                        operand_b,
                    }
                    | Comparator::Less {
                        operand_a,
                        operand_b,
                    }
                    | Comparator::LessOrEqual {
                        operand_a,
                        operand_b,
                    } => (operand_a, operand_b),
                };
                self.expr_contains_user_function_call(a) || self.expr_contains_user_function_call(b)
            }
            Expression::PostfixFieldAccess { expr, .. } => {
                self.expr_contains_user_function_call(expr)
            }
            Expression::Variable(var) => self.expr_contains_user_function_call(&var.value),
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
        // If the template has no ReactiveRef, the reactive value can't be
        // interpolated (e.g., it's used inside a user function body via PASSED).
        // Signal the caller to fall back to the closure-based approach.
        if !doc_template.has_reactive_ref() {
            return Err("Template has no reactive reference".to_string());
        }
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
                        combined.insert(Arc::from(format!("{DEP_FIELD_PREFIX}0")), v.clone());
                        doc_closure(&Value::Object(Arc::new(combined)))
                    }),
                },
            );
            return Ok(doc_var);
        }

        let current_var = self.fresh_var("joined_deps");
        let source_vars: Vec<VarId> = real_dep_names
            .iter()
            .map(|dep_name| self.reactive_vars.get(dep_name).unwrap().clone())
            .collect();
        self.collections.insert(
            current_var.clone(),
            CollectionSpec::CombineLatest(source_vars),
        );

        // Map from joined deps to document
        let doc_closure = self.build_multi_dep_doc_closure(doc_expr, &real_dep_names)?;
        let doc_var = self.fresh_var("document");
        self.collections.insert(
            doc_var.clone(),
            CollectionSpec::Map {
                source: current_var,
                f: Arc::new(move |combined: &Value| doc_closure(combined)),
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
        let keyed_list_short_name = self.keyed_display_list_name.clone();
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
        let mut static_scope_bindings: Vec<(String, Value)> = Vec::new();
        for (var_name, var_expr) in self.compiler.variables.iter() {
            if let Some(field_name) = var_name.strip_prefix("store.") {
                // Skip reactive deps — they'll be provided from the combined value
                if dep_names.iter().any(|d| d == var_name) {
                    continue;
                }
                // Try static evaluation for the default value
                match self
                    .compiler
                    .eval_static_with_scope(var_expr, &IndexMap::new())
                {
                    Ok(val) => {
                        link_defaults.push((field_name.to_string(), val));
                    }
                    Err(_) => {
                        // Can't evaluate statically — skip (LINK evaluates to Value::tag("LINK"))
                    }
                }
                continue;
            }

            if var_name == "document"
                || self.reactive_vars.contains_key(var_name)
                || var_name.contains('.')
            {
                continue;
            }

            if let Ok(val) = self
                .compiler
                .eval_static_with_scope(var_expr, &IndexMap::new())
            {
                if value_needs_reactive_decoration(&val) {
                    static_scope_bindings.push((
                        var_name.clone(),
                        decorate_reactive_scope_value(&val, var_name),
                    ));
                }
            }
        }

        // Collect LINK variable paths for injection into the store object.
        // These create __link_path__ fields so the bridge can route events back.
        let mut link_injections: Vec<(String, String)> = Vec::new(); // (nested_path, full_path)
        for (var_name, var_expr) in self.compiler.variables.iter() {
            if matches!(var_expr.node, Expression::Link) {
                if let Some(rest) = var_name.strip_prefix("store.") {
                    link_injections.push((rest.to_string(), var_name.clone()));
                }
            }
        }

        Ok(Arc::new(move |combined: &Value| {
            let mut scope = IndexMap::new();

            for (name, value) in &static_scope_bindings {
                scope.insert(name.clone(), value.clone());
            }

            // Reconstruct the scope from combined deps.
            // Deps like "store.todos", "theme_options.mode" are grouped into parent
            // objects ("store", "theme_options") for the static evaluator.
            let mut object_fields: std::collections::HashMap<String, BTreeMap<Arc<str>, Value>> =
                std::collections::HashMap::new();

            for (i, name) in dep_names.iter().enumerate() {
                let dep_key = format!("{DEP_FIELD_PREFIX}{i}");
                let val = combined.get_field(&dep_key).cloned().unwrap_or(Value::Unit);
                let decorated = decorate_reactive_scope_value(&val, name);

                // Parse dotted names: "store.items" → group into parent "store"
                let parts: Vec<&str> = name.split('.').collect();
                if parts.len() == 2 {
                    object_fields
                        .entry(parts[0].to_string())
                        .or_default()
                        .insert(Arc::from(parts[1]), decorated.clone());
                }
                scope.insert(name.clone(), decorated);
            }

            // Build composite objects for each parent prefix
            for (parent_name, fields) in &object_fields {
                if parent_name == "store" {
                    // store gets special handling below (LINK stubs, link path injection)
                    continue;
                }
                scope.insert(parent_name.clone(), Value::Object(Arc::new(fields.clone())));
            }

            let mut store_fields = object_fields.remove("store").unwrap_or_default();

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
                    store_value = set_nested_field(
                        &store_value,
                        nested_path,
                        Value::object([(LINK_PATH_FIELD, Value::text(full_path.as_str()))]),
                    );
                }

                // Inject per-item link paths for list fields (todo items need keyed paths
                // like "store.todos.0001.todo_elements.todo_checkbox" for wildcard routing)
                if let Value::Object(ref fields) = store_value {
                    let fields_snapshot = fields.clone();
                    for (field_name, field_val) in fields_snapshot.iter() {
                        if let Value::Tagged {
                            tag,
                            fields: list_fields,
                        } = field_val
                        {
                            if tag.as_ref() == LIST_TAG {
                                let list_path = format!("store.{}", field_name);
                                let new_list_fields: BTreeMap<Arc<str>, Value> = list_fields
                                    .iter()
                                    .map(|(key, item)| {
                                        let new_item = inject_item_link_paths_with_key(
                                            item,
                                            &list_path,
                                            key.as_ref(),
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

            // Propagate keyed list name through the scope so that eval_element_static
            // can mark the matching Stripe with __keyed__: True at evaluation time.
            if let Some(ref kln) = keyed_list_short_name {
                scope.insert(KEYED_LIST_NAME_FIELD.to_string(), Value::text(kln.as_str()));
            }

            #[cfg(test)]
            if dep_names.iter().any(|name| name == "sheet") {
                if let Some(sheet) = scope.get("sheet") {
                    let row_1 = sheet
                        .list_items()
                        .first()
                        .copied()
                        .map(Value::list_items)
                        .unwrap_or_default();
                    let preview = row_1
                        .iter()
                        .take(3)
                        .map(|value| value.as_text().unwrap_or("?").to_string())
                        .collect::<Vec<_>>()
                        .join("|");
                    eprintln!("[cells-doc-scope] row1={preview}");
                }
            }

            match compiler.eval_static_with_scope(&doc_expr, &scope) {
                Ok(val) => val,
                Err(err) => {
                    #[cfg(target_arch = "wasm32")]
                    zoon::eprintln!("[DD DOC STRICT FALLBACK] {err}");
                    #[cfg(not(target_arch = "wasm32"))]
                    std::eprintln!("[DD DOC STRICT FALLBACK] {err}");
                    compiler.eval_static_tolerant(&doc_expr, &scope)
                }
            }
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
                let (then_body, event_source_name) = match &body.node {
                    Expression::Pipe { from, to } => match &to.node {
                        Expression::Then { body: then_body } => (
                            Some(then_body.as_ref().clone()),
                            Self::extract_hold_event_source_name(from),
                        ),
                        _ => (None, None),
                    },
                    _ => (None, None),
                };
                if let Some(then_expr) = then_body {
                    let compiler = self.compiler.clone();
                    let sname = state_name.to_string();
                    let event_name = event_source_name.clone();
                    Arc::new(move |state: &Value, event: &Value| {
                        if matches!(event, Value::Unit) || event.get_tag() == Some("SKIP") {
                            return state.clone();
                        }
                        let mut scope = IndexMap::new();
                        scope.insert(sname.clone(), state.clone());
                        if let Some(event_name) = &event_name {
                            inject_scope_path_merged(&mut scope, event_name, event.clone());
                        }
                        let result = compiler
                            .eval_static_with_scope(&then_expr, &scope)
                            .unwrap_or_else(|err| {
                                #[cfg(not(target_arch = "wasm32"))]
                                std::eprintln!(
                                    "[dd-hold-transform-error] state_name={} event_name={:?} err={} state={} event={}",
                                    sname,
                                    event_name,
                                    err,
                                    state,
                                    event
                                );
                                if event.get_tag() != Some("Event") {
                                    event.clone()
                                } else {
                                    // Last resort fallback: increment by 1
                                    let current = state.as_number().unwrap_or(0.0);
                                    Value::number(current + 1.0)
                                }
                            });
                        #[cfg(not(target_arch = "wasm32"))]
                        if sname == "sheet" || sname == "overrides" {
                            std::eprintln!(
                                "[dd-hold-transform] state_name={} event_name={:?} event={} result={}",
                                sname,
                                event_name,
                                event,
                                result
                            );
                        }
                        result
                    })
                } else if let Expression::Latest { inputs } = &body.node {
                    // LATEST body: merge multiple event sources.
                    // Constant THEN bodies produce the new value directly.
                    // State-dependent THEN bodies produce Value::tag("Event") markers.
                    // For markers, we evaluate THEN bodies with state in scope.
                    let then_bodies: Vec<Spanned<Expression>> = inputs
                        .iter()
                        .filter_map(|input| {
                            if let Expression::Pipe { to, .. } = &input.node {
                                if let Expression::Then { body } = &to.node {
                                    return Some(body.as_ref().clone());
                                }
                            }
                            None
                        })
                        .collect();
                    let compiler = self.compiler.clone();
                    let sname = state_name.to_string();
                    Arc::new(move |state: &Value, event: &Value| {
                        if matches!(event, Value::Unit) || event.get_tag() == Some("SKIP") {
                            return state.clone();
                        }
                        // If event is a concrete value (not marker), use it directly
                        if event.get_tag() != Some("Event") {
                            return event.clone();
                        }
                        // Event marker: evaluate state-dependent THEN bodies with state
                        let mut scope = IndexMap::new();
                        scope.insert(sname.clone(), state.clone());
                        for then_body in &then_bodies {
                            if let Ok(result) = compiler.eval_static_with_scope(then_body, &scope) {
                                // SKIP means don't update state
                                if result.get_tag() == Some("SKIP") {
                                    return state.clone();
                                }
                                return result;
                            }
                        }
                        // Fallback: keep current state
                        state.clone()
                    })
                } else {
                    // No THEN body found — event replaces state
                    Arc::new(|_state: &Value, event: &Value| event.clone())
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

    fn extract_hold_event_source_name(expr: &Spanned<Expression>) -> Option<String> {
        match &expr.node {
            Expression::Alias(Alias::WithoutPassed { parts, .. }) => {
                let full_path = parts
                    .iter()
                    .map(|part| part.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                Some(Self::normalize_event_binding_path(&full_path))
            }
            _ => None,
        }
    }

    fn normalize_event_binding_path(full_path: &str) -> String {
        if full_path.contains(".event.") {
            let (_, link_path) = Self::detect_event_kind_and_path(full_path);
            link_path
        } else {
            full_path.to_string()
        }
    }

    fn retain_non_event_reactive_deps(
        reactive_deps: &mut Vec<String>,
        event_source_name: Option<&String>,
    ) {
        if let Some(event_source_name) = event_source_name {
            let event_prefix = format!("{event_source_name}.");
            reactive_deps.retain(|dep| dep != event_source_name && !dep.starts_with(&event_prefix));
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

    fn build_then_dep_transform(
        &self,
        body: &Spanned<Expression>,
        dep_names: &[String],
        relative_prefix: Option<String>,
        event_source_name: Option<String>,
    ) -> Arc<dyn Fn(&Value) -> Value + 'static> {
        let compiler = self.compiler.clone();
        let body = body.clone();
        let dep_names = dep_names.to_vec();
        let event_source_name = event_source_name.clone();
        Arc::new(move |combined: &Value| {
            let mut scope = IndexMap::new();
            let mut relative_scope = Value::Object(Arc::new(BTreeMap::new()));
            let event_value = combined
                .get_field("__event")
                .cloned()
                .unwrap_or_else(|| combined.clone());

            if matches!(event_value, Value::Unit) || event_value.get_tag() == Some("SKIP") {
                return Value::Unit;
            }

            scope.insert("__event".to_string(), event_value.clone());
            if let Some(event_source_name) = &event_source_name {
                inject_scope_path_merged(&mut scope, event_source_name, event_value);
            }

            for (i, name) in dep_names.iter().enumerate() {
                let dep_key = format!("{DEP_FIELD_PREFIX}{i}");
                let value = combined.get_field(&dep_key).cloned().unwrap_or(Value::Unit);
                scope.insert(name.clone(), value.clone());
                inject_scope_path_merged(&mut scope, name, value.clone());
                if let Some(prefix) = relative_prefix.as_deref() {
                    if let Some(relative_path) = name.strip_prefix(&format!("{prefix}.")) {
                        relative_scope =
                            set_nested_field(&relative_scope, relative_path, value.clone());
                    }
                }
            }

            if let Value::Object(fields) = relative_scope {
                for (field_name, value) in fields.iter() {
                    scope.insert(field_name.to_string(), value.clone());
                }
            }

            let result = compiler.eval_static_tolerant(&body, &scope);
            #[cfg(not(target_arch = "wasm32"))]
            if dep_names.iter().any(|name| name.contains("is_return"))
                || dep_names.iter().any(|name| name.contains("departure_date"))
            {
                std::eprintln!(
                    "[dd-then-deps] deps={:?} combined={} result={}",
                    dep_names,
                    combined,
                    result
                );
            }
            #[cfg(not(target_arch = "wasm32"))]
            if event_source_name.as_deref() == Some("enter_pressed")
                || event_source_name.as_deref() == Some("edit_changed")
            {
                std::eprintln!(
                    "[dd-then-event] event_name={:?} deps={:?} combined={} result={}",
                    event_source_name,
                    dep_names,
                    combined,
                    result
                );
            }
            result
        })
    }

    fn join_event_with_reactive_deps(
        &mut self,
        event_var: VarId,
        reactive_deps: &[String],
    ) -> Result<VarId, String> {
        let first_dep = reactive_deps
            .first()
            .ok_or_else(|| "join_event_with_reactive_deps requires at least one dep".to_string())?;
        let first_dep_var = if let Some(var_id) = self.reactive_vars.get(first_dep).cloned() {
            var_id
        } else if self
            .compiler
            .get_var_expr(first_dep)
            .is_some_and(|expr| self.compiler.is_reactive(expr))
        {
            VarId::new(first_dep.as_str())
        } else {
            return Err(format!("Reactive dep '{}' not found", first_dep));
        };

        let mut current_var = self.fresh_var("then_deps_join");
        self.collections.insert(
            current_var.clone(),
            CollectionSpec::SampleOnEvent {
                event: event_var,
                dep: first_dep_var,
                f: Arc::new(move |event: &Value, dep: &Value| {
                    let mut fields = BTreeMap::new();
                    fields.insert(Arc::from("__event"), event.clone());
                    fields.insert(Arc::from(format!("{DEP_FIELD_PREFIX}0")), dep.clone());
                    Value::Object(Arc::new(fields))
                }),
            },
        );

        for (i, dep_name) in reactive_deps.iter().enumerate().skip(1) {
            let dep_var = if let Some(var_id) = self.reactive_vars.get(dep_name).cloned() {
                var_id
            } else if self
                .compiler
                .get_var_expr(dep_name)
                .is_some_and(|expr| self.compiler.is_reactive(expr))
            {
                VarId::new(dep_name.as_str())
            } else {
                return Err(format!("Reactive dep '{}' not found", dep_name));
            };
            let prev = current_var.clone();
            current_var = self.fresh_var("then_deps_join");
            let idx = i;
            self.collections.insert(
                current_var.clone(),
                CollectionSpec::SampleOnEvent {
                    event: prev,
                    dep: dep_var,
                    f: Arc::new(move |combined: &Value, dep: &Value| {
                        combined.update_field(&format!("{DEP_FIELD_PREFIX}{idx}"), dep.clone())
                    }),
                },
            );
        }

        Ok(current_var)
    }

    fn collect_ensured_text_reactive_deps(
        &mut self,
        expr: &Spanned<Expression>,
        deps: &mut Vec<String>,
    ) {
        if let Some(dep) = self.ensure_text_alias_reactive_dep(expr) {
            if !deps.contains(&dep) {
                deps.push(dep);
            }
        }
        match &expr.node {
            Expression::Pipe { from, to } => {
                self.collect_ensured_text_reactive_deps(from, deps);
                self.collect_ensured_text_reactive_deps(to, deps);
            }
            Expression::FunctionCall { arguments, .. } => {
                for arg in arguments {
                    if let Some(value) = arg.node.value.as_ref() {
                        self.collect_ensured_text_reactive_deps(value, deps);
                    }
                }
            }
            Expression::Block {
                variables, output, ..
            } => {
                for var in variables {
                    self.collect_ensured_text_reactive_deps(&var.node.value, deps);
                }
                self.collect_ensured_text_reactive_deps(output, deps);
            }
            Expression::When { arms } | Expression::While { arms } => {
                for arm in arms {
                    self.collect_ensured_text_reactive_deps(&arm.body, deps);
                }
            }
            Expression::List { items } => {
                for item in items {
                    self.collect_ensured_text_reactive_deps(item, deps);
                }
            }
            Expression::Object(object) => {
                for var in &object.variables {
                    self.collect_ensured_text_reactive_deps(&var.node.value, deps);
                }
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
                } => {
                    self.collect_ensured_text_reactive_deps(operand_a, deps);
                    self.collect_ensured_text_reactive_deps(operand_b, deps);
                }
                ArithmeticOperator::Negate { operand } => {
                    self.collect_ensured_text_reactive_deps(operand, deps);
                }
            },
            Expression::Comparator(cmp) => match cmp {
                static_expression::Comparator::Equal {
                    operand_a,
                    operand_b,
                }
                | static_expression::Comparator::NotEqual {
                    operand_a,
                    operand_b,
                }
                | static_expression::Comparator::Greater {
                    operand_a,
                    operand_b,
                }
                | static_expression::Comparator::Less {
                    operand_a,
                    operand_b,
                }
                | static_expression::Comparator::GreaterOrEqual {
                    operand_a,
                    operand_b,
                }
                | static_expression::Comparator::LessOrEqual {
                    operand_a,
                    operand_b,
                } => {
                    self.collect_ensured_text_reactive_deps(operand_a, deps);
                    self.collect_ensured_text_reactive_deps(operand_b, deps);
                }
            },
            _ => {}
        }
    }

    /// Resolve an alias expression to its reactive VarId.
    /// Check if any argument in a built-in piped function references a reactive variable.
    /// Returns the first reactive argument's name and VarId, if any.
    fn find_first_reactive_argument(
        &self,
        args: &[(String, Option<Spanned<Expression>>)],
    ) -> Option<(String, VarId)> {
        for (arg_name, arg_expr_opt) in args {
            if let Some(arg_expr) = arg_expr_opt {
                if let Expression::Alias(Alias::WithoutPassed { parts, .. }) = &arg_expr.node {
                    if parts.len() == 1 {
                        let name = parts[0].as_str();
                        if let Some(var_id) = self.reactive_vars.get(name) {
                            return Some((arg_name.clone(), var_id.clone()));
                        }
                        // Try with scope prefix
                        if let Some(prefix) = &self.scope_prefix {
                            let prefixed = format!("{}.{}", prefix, name);
                            if let Some(var_id) = self.reactive_vars.get(&prefixed) {
                                return Some((arg_name.clone(), var_id.clone()));
                            }
                        }
                    }
                }
            }
        }
        None
    }

    fn resolve_reactive_source(&mut self, expr: &Spanned<Expression>) -> Result<VarId, String> {
        if let Expression::Alias(Alias::WithoutPassed { parts, .. }) = &expr.node {
            let full_name = parts
                .iter()
                .map(|part| part.as_str())
                .collect::<Vec<_>>()
                .join(".");

            if let Some(var_id) = self.reactive_vars.get(&full_name) {
                return Ok(var_id.clone());
            }

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
                    if let Some(prefixed_expr) = self.compiler.get_var_expr(&prefixed) {
                        if self.compiler.is_reactive(prefixed_expr) {
                            return Err(format!("Reactive source '{}' not compiled yet", prefixed));
                        }
                    }
                }
                if let Some(var_expr) = self.compiler.get_var_expr(name) {
                    if self.compiler.is_reactive(var_expr) {
                        return Err(format!("Reactive source '{}' not compiled yet", name));
                    }
                }
            }
            if let Some(var_expr) = self.compiler.get_var_expr(&full_name) {
                if self.compiler.is_reactive(var_expr) {
                    return Err(format!("Reactive source '{}' not compiled yet", full_name));
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
                    let input_id =
                        self.add_input(InputKind::Router, Some(ROUTER_INPUT.to_string()));
                    let router_var = self.fresh_var(ROUTER_INPUT);
                    self.collections
                        .insert(router_var.clone(), CollectionSpec::Input(input_id));
                    self.reactive_vars
                        .insert(ROUTER_INPUT.to_string(), router_var.clone());
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
        // Handle ArithmeticOperator as reactive source (e.g., `elapsed * 10` in a pipe)
        if let Expression::ArithmeticOperator(op) = &expr.node {
            let temp_name = self.fresh_var("__arith_source");
            let var_id = self.compile_reactive_arithmetic(temp_name.as_str(), op)?;
            return Ok(var_id);
        }
        // Handle Comparator as reactive source (e.g., `a < b` in a pipe)
        if let Expression::Comparator(cmp) = &expr.node {
            let temp_name = self.fresh_var("__cmp_source");
            let var_id = self.compile_reactive_comparison(temp_name.as_str(), cmp)?;
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
                        // Wildcard or named binding — always matches.
                        // If the body couldn't be statically evaluated (is Unit),
                        // return the input value (pass-through for binding patterns
                        // like `click => [x: click.x, y: click.y]`).
                        if *body_val == Value::Unit {
                            return input.clone();
                        }
                        return body_val.clone();
                    }
                }
            }
            Value::Unit
        })
    }

    /// Build a WHEN pattern-matching closure for FlatMap collections.
    /// Returns `Some(value)` for matching arms, `None` for SKIP arms.
    /// Used inside LATEST to avoid updating the held value on SKIP.
    fn build_when_flatmap_fn(
        &self,
        arms: &[static_expression::Arm],
    ) -> Arc<dyn Fn(Value) -> Option<Value> + 'static> {
        // Pre-compile arms: (pattern_value, body_value, is_skip).
        let mut compiled_arms: Vec<(Option<Value>, Value, bool)> = Vec::new();

        for arm in arms {
            let pattern_val = self.try_eval_pattern_to_value(&arm.pattern);
            let is_skip = matches!(arm.body.node, Expression::Skip);
            let body_val = self.compiler.eval_static(&arm.body).unwrap_or(Value::Unit);
            compiled_arms.push((pattern_val, body_val, is_skip));
        }

        Arc::new(move |input: Value| {
            for (pattern_val, body_val, is_skip) in &compiled_arms {
                match pattern_val {
                    Some(pv) => {
                        if input == *pv {
                            return if *is_skip {
                                None
                            } else {
                                Some(body_val.clone())
                            };
                        }
                    }
                    None => {
                        // Wildcard — always matches
                        return if *is_skip {
                            None
                        } else {
                            Some(body_val.clone())
                        };
                    }
                }
            }
            None // No arm matched — suppress value
        })
    }

    /// Try to convert a WHEN pattern to a concrete Value for equality matching.
    /// Returns None for wildcards/bindings (which match anything).
    fn try_eval_pattern_to_value(&self, pattern: &static_expression::Pattern) -> Option<Value> {
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
    fn find_root_and_derived(&self, deps: &[String]) -> Result<(String, Vec<String>), String> {
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
                Self::expr_references_var(from, var_name) || Self::expr_references_var(to, var_name)
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
        let has_skip = arms
            .iter()
            .any(|arm| matches!(arm.body.node, Expression::Skip));
        if !has_skip {
            return false;
        }
        // Check if from is a key_down event path
        if let Expression::Alias(Alias::WithoutPassed { parts, .. }) = &from.node {
            let path: String = parts
                .iter()
                .map(|p| p.as_str())
                .collect::<Vec<_>>()
                .join(".");
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
            Expression::Alias(Alias::WithoutPassed { parts, .. }) => parts
                .iter()
                .map(|p| p.as_str())
                .collect::<Vec<_>>()
                .join("."),
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
        self.collections
            .insert(key_input_var.clone(), CollectionSpec::Input(key_input_id));

        // Create TextChange input for the associated text input
        // Derive the change event path from the key_down path:
        // "store.X.event.key_down" → "store.X.event.change"
        let change_path = key_link_path.replace(".event.key_down", ".event.change");
        let text_input_id = self.add_input(InputKind::TextChange, Some(change_path));
        let text_input_var = self.fresh_var("text_change_input");
        self.collections
            .insert(text_input_var.clone(), CollectionSpec::Input(text_input_id));

        // Tag the text change events: Value::text(text) → [__t: "text", v: text]
        let tagged_text_var = self.fresh_var("tagged_text");
        self.collections.insert(
            tagged_text_var.clone(),
            CollectionSpec::Map {
                source: text_input_var,
                f: Arc::new(|v: &Value| {
                    let text_value = v.get_field("text").cloned().unwrap_or_else(|| v.clone());
                    Value::object([("__t", Value::text("text")), ("v", v.clone())])
                        .update_field("v", text_value)
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
                    let key_value = v.get_field("key").cloned().unwrap_or_else(|| v.clone());
                    Value::object([("__t", Value::text("key")), ("v", key_value)])
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
        let initial_state = Value::object([("display", Value::text("")), ("pending", Value::Unit)]);
        let initial_var = self.fresh_var("text_state_init");
        self.collections.insert(
            initial_var.clone(),
            CollectionSpec::Literal(initial_state.clone()),
        );

        let hold_var = self.fresh_var("text_key_state");
        self.collections.insert(
            hold_var.clone(),
            CollectionSpec::HoldState {
                initial: initial_var,
                events: combined_var,
                initial_value: initial_state,
                transform: Arc::new(|state: &Value, event: &Value| {
                    let event_type = event
                        .get_field("__t")
                        .and_then(|v| v.as_text())
                        .unwrap_or("")
                        .to_string();
                    match event_type.as_str() {
                        "text" => {
                            // Text change: update display text, clear pending
                            let text = event.get_field("v").cloned().unwrap_or(Value::text(""));
                            Value::object([("display", text), ("pending", Value::Unit)])
                        }
                        "key" => {
                            let key = event
                                .get_field("v")
                                .and_then(|v| v.as_text())
                                .unwrap_or("")
                                .to_string();
                            if key == "Enter" {
                                let display = state
                                    .get_field("display")
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
                                let display = state
                                    .get_field("display")
                                    .cloned()
                                    .unwrap_or(Value::text(""));
                                Value::object([("display", display), ("pending", Value::Unit)])
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
        self.reactive_vars
            .insert(format!("{}.__state", name), hold_var);

        self.reactive_vars
            .insert(name.to_string(), flatmap_var.clone());
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
            self.collections
                .insert(count_var.clone(), CollectionSpec::ListCount(keyed_var));
            self.reactive_vars
                .insert(name.to_string(), count_var.clone());
            return Ok(count_var);
        }

        // Case 2: Keyed source through retain pipe
        // Pattern: `keyed_source |> List/retain(item, if: predicate) |> List/count()`
        // Uses HoldState(initial=0, events=ListCount) because ListRetain may produce
        // empty results, and ListCount on empty keyed collection never fires.
        if let Expression::Pipe {
            from: inner_from,
            to: inner_to,
        } = &from.node
        {
            if let Expression::FunctionCall { path, arguments } = &inner_to.node {
                let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                if path_strs.as_slice() == ["List", "retain"] {
                    if let Some(keyed_var) = self.resolve_keyed_source(inner_from) {
                        let retain_var =
                            self.compile_inline_keyed_retain(keyed_var, arguments, &[])?;
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
        self.reactive_vars
            .insert(name.to_string(), count_var.clone());
        Ok(count_var)
    }

    /// Compile `source |> List/latest()`.
    ///
    /// Reduces a keyed `(ListKey, Value)` collection to a scalar `Value`
    /// that always holds the most recently changed item.
    fn try_compile_inline_keyed_event_map(
        &mut self,
        name: &str,
        source_expr: &Spanned<Expression>,
        keyed_var: VarId,
        arguments: &[Spanned<Argument>],
    ) -> Result<Option<VarId>, String> {
        let item_param = arguments
            .iter()
            .find(|a| a.node.value.is_none())
            .map(|a| a.node.name.as_str().to_string())
            .unwrap_or_else(|| "item".to_string());

        let new_expr = arguments
            .iter()
            .find(|a| a.node.name.as_str() == "new")
            .and_then(|a| a.node.value.as_ref());

        let Some(new_expr) = new_expr else {
            return Ok(None);
        };

        let Expression::Pipe { from, to } = &new_expr.node else {
            return Ok(None);
        };
        let Expression::Then { body } = &to.node else {
            return Ok(None);
        };

        let Some(event_suffix) = Self::extract_item_event_suffix(from, &item_param) else {
            return Ok(None);
        };
        let event_scope_path =
            Self::normalize_event_binding_path(&format!("{item_param}.{event_suffix}"));
        let Some(list_path) = self.resolve_keyed_source_path(source_expr) else {
            return Ok(None);
        };

        let wildcard_id = self.add_input(InputKind::LinkPress, Some("__wildcard".to_string()));
        let wildcard_input_var = self.fresh_var("wildcard_input");
        self.collections.insert(
            wildcard_input_var.clone(),
            CollectionSpec::Input(wildcard_id),
        );

        let list_prefix = format!("{list_path}.");
        let wildcard_keyed_var = self.fresh_var("wildcard_keyed");
        self.collections.insert(
            wildcard_keyed_var.clone(),
            CollectionSpec::MapToKeyed {
                source: wildcard_input_var,
                classify: Arc::new(move |value: &Value| {
                    let path = value.get_field("path")?.as_text()?;
                    let event_value = value.get_field("value").cloned().unwrap_or(Value::Unit);
                    let rest = path.strip_prefix(&list_prefix)?;
                    let mut parts = rest.splitn(2, '.');
                    let key = parts.next()?;
                    let subpath = parts.next()?;
                    if subpath == event_suffix {
                        Some((ListKey::new(key), event_value))
                    } else {
                        None
                    }
                }),
            },
        );

        let compiler = self.compiler.clone();
        let body = body.clone();
        let item_param_clone = item_param.clone();
        let event_scope_path_clone = event_scope_path.clone();
        let event_map_var = VarId::new(name);
        self.collections.insert(
            event_map_var.clone(),
            CollectionSpec::KeyedEventMap {
                items: keyed_var,
                events: wildcard_keyed_var,
                f: Arc::new(move |item: &Value, event: &Value| {
                    let mut scope = IndexMap::new();
                    scope.insert(item_param_clone.clone(), item.clone());
                    scope.insert("__event".to_string(), event.clone());
                    scope.insert("event_value".to_string(), event.clone());
                    inject_scope_path_merged(&mut scope, &event_scope_path_clone, event.clone());
                    compiler.eval_static_tolerant(&body, &scope)
                }),
            },
        );
        self.reactive_vars
            .insert(name.to_string(), event_map_var.clone());
        Ok(Some(event_map_var))
    }

    fn try_compile_inline_static_event_map(
        &mut self,
        name: &str,
        source_expr: &Spanned<Expression>,
        arguments: &[Spanned<Argument>],
    ) -> Result<Option<VarId>, String> {
        let item_param = arguments
            .iter()
            .find(|a| a.node.value.is_none())
            .map(|a| a.node.name.as_str().to_string())
            .unwrap_or_else(|| "item".to_string());

        let new_expr = arguments
            .iter()
            .find(|a| a.node.name.as_str() == "new")
            .and_then(|a| a.node.value.as_ref());

        let Some(new_expr) = new_expr else {
            return Ok(None);
        };

        let Expression::Pipe { from, to } = &new_expr.node else {
            return Ok(None);
        };

        let source_value = match self
            .compiler
            .eval_static_with_scope(source_expr, &IndexMap::new())
        {
            Ok(value) => value,
            Err(_) => return Ok(None),
        };

        let source_name = Self::resolve_static_source_name(source_expr);
        let event_scope_path = Self::extract_item_event_suffix(from, &item_param)
            .map(|suffix| Self::normalize_event_binding_path(&format!("{item_param}.{suffix}")));
        let compiler = self.compiler.clone();
        let mut event_vars = Vec::new();

        for (idx, item) in source_value.list_items().into_iter().enumerate() {
            let key = format!("{idx:04}");
            let decorated_item = if let Some(source_name) = source_name.as_deref() {
                decorate_reactive_scope_value(&item, &format!("{source_name}.{key}"))
            } else {
                item.clone()
            };

            let Some((kind, link_path)) =
                Self::resolve_static_item_event_path(from, &item_param, &decorated_item)
            else {
                continue;
            };

            let input_id = self.add_input(kind, Some(link_path));
            let input_var = self.fresh_var("static_item_event_input");
            self.collections
                .insert(input_var.clone(), CollectionSpec::Input(input_id));

            let event_var = self.fresh_var("static_item_event_map");
            let item_scope_value = decorated_item.clone();
            let item_param_clone = item_param.clone();
            let from_expr_clone = from.clone();
            let to_expr_clone = to.clone();
            let compiler_clone = compiler.clone();
            let event_scope_path_clone = event_scope_path.clone();
            self.collections.insert(
                event_var.clone(),
                CollectionSpec::Map {
                    source: input_var,
                    f: Arc::new(move |event: &Value| {
                        let mut scope = IndexMap::new();
                        scope.insert(item_param_clone.clone(), item_scope_value.clone());
                        scope.insert("__event".to_string(), event.clone());
                        scope.insert("event_value".to_string(), event.clone());
                        if let Some(event_scope_path) = &event_scope_path_clone {
                            inject_scope_path_merged(&mut scope, event_scope_path, event.clone());
                        }
                        let result = Self::eval_static_event_projection(
                            &compiler_clone,
                            &scope,
                            &from_expr_clone,
                            &to_expr_clone,
                        );
                        #[cfg(not(target_arch = "wasm32"))]
                        if item_param_clone == "cell" || item_param_clone == "item" {
                            std::eprintln!(
                                "[dd-static-item-event] item_param={} event_scope={:?} event={} scope_item={} result={}",
                                item_param_clone,
                                event_scope_path_clone,
                                event,
                                scope.get(&item_param_clone).cloned().unwrap_or(Value::Unit),
                                result
                            );
                        }
                        result
                    }),
                },
            );
            event_vars.push(event_var);
        }

        if event_vars.is_empty() {
            return Ok(None);
        }

        let result_var = if event_vars.len() == 1 {
            event_vars.pop().expect("single event var")
        } else {
            let concat_var = VarId::new(name);
            self.collections
                .insert(concat_var.clone(), CollectionSpec::Concat(event_vars));
            concat_var
        };

        self.reactive_vars
            .insert(name.to_string(), result_var.clone());
        Ok(Some(result_var))
    }

    fn literal_expr_from_value(value: &Value) -> Option<Spanned<Expression>> {
        let node = match value {
            Value::Number(n) => Expression::Literal(Literal::Number(**n)),
            Value::Text(text) => {
                let source = SourceCode::new(text.to_string());
                let slice = source.slice(0, text.len());
                Expression::Literal(Literal::Text(slice))
            }
            Value::Tag(tag) => {
                let source = SourceCode::new(tag.to_string());
                let slice = source.slice(0, tag.len());
                Expression::Literal(Literal::Tag(slice))
            }
            _ => return None,
        };

        Some(Spanned {
            span: span_at(0),
            persistence: None,
            node,
        })
    }

    fn eval_static_event_projection(
        compiler: &Compiler,
        scope: &IndexMap<String, Value>,
        from_expr: &Spanned<Expression>,
        to_expr: &Spanned<Expression>,
    ) -> Value {
        if let Expression::Then { body } = &to_expr.node {
            return compiler.eval_static_tolerant(body, scope);
        }

        let from_value = compiler.eval_static_tolerant(from_expr, scope);
        let Some(event_expr) = Self::literal_expr_from_value(&from_value) else {
            return Value::Unit;
        };

        let piped_expr = Spanned {
            span: to_expr.span,
            persistence: None,
            node: Expression::Pipe {
                from: Box::new(event_expr),
                to: Box::new(to_expr.clone()),
            },
        };

        compiler.eval_static_tolerant(&piped_expr, scope)
    }

    fn compile_list_latest(
        &mut self,
        name: &str,
        from: &Spanned<Expression>,
    ) -> Result<VarId, String> {
        // Try keyed source (e.g., `todos |> List/latest()`)
        if let Some(keyed_var) = self.resolve_keyed_source(from) {
            let latest_var = VarId::new(name);
            self.collections
                .insert(latest_var.clone(), CollectionSpec::ListLatest(keyed_var));
            self.reactive_vars
                .insert(name.to_string(), latest_var.clone());
            return Ok(latest_var);
        }

        // Keyed source through pipe chain:
        // `keyed |> List/retain(...) |> List/latest()`
        // `keyed |> List/map(...) |> List/latest()`
        if let Expression::Pipe {
            from: inner_from,
            to: inner_to,
        } = &from.node
        {
            if let Expression::FunctionCall { path, arguments } = &inner_to.node {
                let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                match path_strs.as_slice() {
                    ["List", "retain"] => {
                        if let Some(keyed_var) = self.resolve_keyed_source(inner_from) {
                            let retain_var =
                                self.compile_inline_keyed_retain(keyed_var, arguments, &[])?;
                            let latest_var = VarId::new(name);
                            self.collections
                                .insert(latest_var.clone(), CollectionSpec::ListLatest(retain_var));
                            self.reactive_vars
                                .insert(name.to_string(), latest_var.clone());
                            return Ok(latest_var);
                        }
                    }
                    ["List", "map"] => {
                        if let Some(keyed_var) = self.resolve_keyed_source(inner_from) {
                            if let Some(event_var) = self.try_compile_inline_keyed_event_map(
                                name,
                                inner_from,
                                keyed_var.clone(),
                                arguments,
                            )? {
                                return Ok(event_var);
                            }
                            let map_var = self.compile_inline_keyed_map(keyed_var, arguments)?;
                            let latest_var = VarId::new(name);
                            self.collections
                                .insert(latest_var.clone(), CollectionSpec::ListLatest(map_var));
                            self.reactive_vars
                                .insert(name.to_string(), latest_var.clone());
                            return Ok(latest_var);
                        }
                        if let Some(event_var) =
                            self.try_compile_inline_static_event_map(name, inner_from, arguments)?
                        {
                            return Ok(event_var);
                        }
                    }
                    _ => {}
                }
            }
        }

        Err(format!(
            "List/latest() requires a keyed list source for '{}'",
            name,
        ))
    }

    /// Compile `source |> List/every(item, if: predicate)`.
    ///
    /// Reduces a keyed list to a scalar bool — True if all items match.
    fn compile_list_every(
        &mut self,
        name: &str,
        from: &Spanned<Expression>,
        arguments: &[Spanned<Argument>],
    ) -> Result<VarId, String> {
        self.compile_list_aggregate_bool(name, from, arguments, true)
    }

    /// Compile `source |> List/any(item, if: predicate)`.
    ///
    /// Reduces a keyed list to a scalar bool — True if any item matches.
    fn compile_list_any(
        &mut self,
        name: &str,
        from: &Spanned<Expression>,
        arguments: &[Spanned<Argument>],
    ) -> Result<VarId, String> {
        self.compile_list_aggregate_bool(name, from, arguments, false)
    }

    /// Shared implementation for List/every and List/any.
    /// `is_every=true` → ListEvery, `is_every=false` → ListAny.
    fn compile_list_aggregate_bool(
        &mut self,
        name: &str,
        from: &Spanned<Expression>,
        arguments: &[Spanned<Argument>],
        is_every: bool,
    ) -> Result<VarId, String> {
        let item_param: String = arguments
            .iter()
            .find(|a| a.node.value.is_none())
            .map(|a| a.node.name.as_str().to_string())
            .unwrap_or_else(|| "item".to_string());

        let predicate_expr = arguments
            .iter()
            .find(|a| a.node.name.as_str() == "if")
            .and_then(|a| a.node.value.as_ref())
            .ok_or_else(|| {
                format!(
                    "List/{} missing 'if' argument for '{}'",
                    if is_every { "every" } else { "any" },
                    name
                )
            })?;

        // Resolve keyed source
        let keyed_var = self.resolve_keyed_source(from).ok_or_else(|| {
            format!(
                "List/{} requires a keyed list source for '{}'",
                if is_every { "every" } else { "any" },
                name
            )
        })?;

        // Build predicate closure
        let compiler = self.compiler.clone();
        let pred_expr = predicate_expr.clone();
        let param_name = item_param;
        let predicate: Arc<dyn Fn(&Value) -> bool + 'static> = Arc::new(move |item: &Value| {
            let mut scope = indexmap::IndexMap::new();
            scope.insert(param_name.clone(), item.clone());
            compiler
                .eval_static_with_scope(&pred_expr, &scope)
                .and_then(|v| Ok(v.as_bool().unwrap_or(false)))
                .unwrap_or(false)
        });

        let result_var = VarId::new(name);
        let spec = if is_every {
            CollectionSpec::ListEvery {
                source: keyed_var,
                predicate,
            }
        } else {
            CollectionSpec::ListAny {
                source: keyed_var,
                predicate,
            }
        };
        self.collections.insert(result_var.clone(), spec);
        self.reactive_vars
            .insert(name.to_string(), result_var.clone());
        Ok(result_var)
    }

    /// Compile `source |> List/is_not_empty()` or `source |> List/is_empty()`.
    ///
    /// Composes from ListCount + Map to produce True/False.
    fn compile_list_emptiness_check(
        &mut self,
        name: &str,
        from: &Spanned<Expression>,
        is_not_empty: bool,
    ) -> Result<VarId, String> {
        // Try keyed source first
        if let Some(keyed_var) = self.resolve_keyed_source(from) {
            // ListCount → Map(count → True/False)
            let count_var = self.fresh_var(&format!("{}_count", name));
            self.collections
                .insert(count_var.clone(), CollectionSpec::ListCount(keyed_var));

            let result_var = VarId::new(name);
            self.collections.insert(
                result_var.clone(),
                CollectionSpec::Map {
                    source: count_var,
                    f: Arc::new(move |v: &Value| {
                        let count = v.as_number().unwrap_or(0.0) as i64;
                        let check = if is_not_empty { count > 0 } else { count == 0 };
                        if check {
                            Value::tag("True")
                        } else {
                            Value::tag("False")
                        }
                    }),
                },
            );
            self.reactive_vars
                .insert(name.to_string(), result_var.clone());
            return Ok(result_var);
        }

        // Scalar fallback: compile source and use Map
        let source_var = self.resolve_reactive_source(from)?;
        let result_var = VarId::new(name);
        self.collections.insert(
            result_var.clone(),
            CollectionSpec::Map {
                source: source_var,
                f: Arc::new(move |v: &Value| {
                    let empty = v.list_is_empty();
                    let check = if is_not_empty { !empty } else { empty };
                    if check {
                        Value::tag("True")
                    } else {
                        Value::tag("False")
                    }
                }),
            },
        );
        self.reactive_vars
            .insert(name.to_string(), result_var.clone());
        Ok(result_var)
    }

    /// Compile `initial |> Bool/toggle(when: event_source)`.
    ///
    /// Desugars to: `HoldState { initial, events, transform: |state, _| !state }`.
    fn compile_bool_toggle(
        &mut self,
        name: &str,
        from: &Spanned<Expression>,
        arguments: &[Spanned<Argument>],
    ) -> Result<VarId, String> {
        // Extract `when:` argument
        let when_expr = arguments
            .iter()
            .find(|a| a.node.name.as_str() == "when")
            .and_then(|a| a.node.value.as_ref())
            .ok_or_else(|| format!("Bool/toggle missing 'when' argument for '{}'", name))?;

        // Compile initial value
        let initial_var = self.compile_reactive_var(&format!("{}_initial", name), from)?;

        // Evaluate initial value statically for HoldState
        let initial_value = self
            .compiler
            .eval_static(from)
            .unwrap_or(Value::tag("False"));

        // Compile the `when:` event source
        let (events_var, _) = self.compile_event_source(when_expr)?;

        // Build HoldState that toggles on each event
        let toggle_var = VarId::new(name);
        self.collections.insert(
            toggle_var.clone(),
            CollectionSpec::HoldState {
                initial: initial_var,
                events: events_var,
                initial_value,
                transform: Arc::new(|state: &Value, _event: &Value| {
                    let b = state.as_bool().unwrap_or(false);
                    if b {
                        Value::tag("False")
                    } else {
                        Value::tag("True")
                    }
                }),
            },
        );
        self.reactive_vars
            .insert(name.to_string(), toggle_var.clone());
        Ok(toggle_var)
    }

    /// Build a keyed ListCount with HoldState for empty-safety.
    ///
    /// Creates: `HoldState(initial=Literal(0), events=ListCount(keyed_var))`.
    /// - At t=0: HoldState starts with initial_value=0
    /// - If ListCount fires (non-empty retain result): state updates to actual count
    /// - If ListCount doesn't fire (empty retain result): state stays at 0
    fn build_empty_safe_keyed_count(
        &mut self,
        name: &str,
        keyed_var: VarId,
    ) -> Result<VarId, String> {
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
        self.reactive_vars
            .insert(name.to_string(), count_var.clone());
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
        scope_bindings: &[(String, Spanned<Expression>)],
    ) -> Result<VarId, String> {
        let item_param: String = arguments
            .iter()
            .find(|a| a.node.value.is_none())
            .map(|a| a.node.name.as_str().to_string())
            .unwrap_or_else(|| "item".to_string());

        let predicate_expr = arguments
            .iter()
            .find(|a| a.node.name.as_str() == "if")
            .and_then(|a| a.node.value.as_ref())
            .ok_or_else(|| "List/retain missing 'if' argument".to_string())?;

        let mut reactive_deps = self.find_reactive_deps_in_expr(predicate_expr);
        for (_, binding_expr) in scope_bindings {
            for dep in self.find_reactive_deps_in_expr(binding_expr) {
                if !reactive_deps.contains(&dep) {
                    reactive_deps.push(dep);
                }
            }
        }
        if reactive_deps.is_empty() {
            for (_, binding_expr) in scope_bindings {
                if let Some(dep) = self.ensure_text_alias_reactive_dep(binding_expr) {
                    reactive_deps.push(dep);
                    break;
                }
            }
        }
        if reactive_deps.is_empty() {
            if let Some(dep) = self.ensure_text_alias_reactive_dep(predicate_expr) {
                reactive_deps.push(dep);
            }
        }

        let retain_var = self.fresh_var("keyed_retain");

        if reactive_deps.is_empty() {
            // Static predicate → CollectionSpec::ListRetain
            let compiler = self.compiler.clone();
            let pred_expr = predicate_expr.clone();
            let param_name = item_param;
            let scope_bindings = scope_bindings.to_vec();

            self.collections.insert(
                retain_var.clone(),
                CollectionSpec::ListRetain {
                    source: keyed_var,
                    predicate: Arc::new(move |item: &Value| {
                        let mut scope = indexmap::IndexMap::new();
                        scope.insert(param_name.clone(), item.clone());
                        compiler.populate_scope_bindings_tolerant(&mut scope, &scope_bindings);
                        compiler
                            .eval_static_with_scope(&pred_expr, &scope)
                            .and_then(|v| Ok(v.as_bool().unwrap_or(false)))
                            .unwrap_or(false)
                    }),
                },
            );
        } else {
            // Reactive predicate → CollectionSpec::ListRetainReactive
            let reactive_dep = reactive_deps[0].clone();
            let reactive_var = self
                .reactive_vars
                .get(&reactive_dep)
                .cloned()
                .ok_or_else(|| format!("List/retain: reactive dep '{}' not found", reactive_dep))?;

            let compiler = self.compiler.clone();
            let pred_expr = predicate_expr.clone();
            let param_name = item_param;
            let scope_bindings = scope_bindings.to_vec();
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
                        scope.insert(PASSED_VAR.to_string(), passed_val);
                        compiler.populate_scope_bindings_tolerant(&mut scope, &scope_bindings);
                        compiler
                            .eval_static_with_scope(&pred_expr, &scope)
                            .and_then(|v| Ok(v.as_bool().unwrap_or(false)))
                            .unwrap_or(false)
                    }),
                },
            );
        }

        self.keyed_collection_vars.insert(retain_var.clone());
        Ok(retain_var)
    }

    fn ensure_text_alias_reactive_dep(&mut self, expr: &Spanned<Expression>) -> Option<String> {
        match &expr.node {
            Expression::Alias(Alias::WithoutPassed { parts, .. }) => {
                let path = parts
                    .iter()
                    .map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                self.ensure_text_state_var(&path)
            }
            Expression::Alias(Alias::WithPassed { extra_parts }) => {
                let mut parts = vec![PASSED_VAR.to_string()];
                parts.extend(extra_parts.iter().map(|p| p.as_str().to_string()));
                self.ensure_text_state_var(&parts.join("."))
            }
            Expression::Pipe { from, to } => self
                .ensure_text_alias_reactive_dep(from)
                .or_else(|| self.ensure_text_alias_reactive_dep(to)),
            Expression::FunctionCall { arguments, .. } => arguments
                .iter()
                .filter_map(|arg| arg.node.value.as_ref())
                .find_map(|value| self.ensure_text_alias_reactive_dep(value)),
            Expression::Block {
                variables, output, ..
            } => variables
                .iter()
                .find_map(|var| self.ensure_text_alias_reactive_dep(&var.node.value))
                .or_else(|| self.ensure_text_alias_reactive_dep(output)),
            Expression::When { arms } | Expression::While { arms } => arms
                .iter()
                .find_map(|arm| self.ensure_text_alias_reactive_dep(&arm.body)),
            _ => None,
        }
    }

    fn ensure_text_state_var(&mut self, path: &str) -> Option<String> {
        let dep_name = path.strip_prefix("PASSED.").unwrap_or(path);
        let dep_name = dep_name.strip_prefix("__passed.").unwrap_or(dep_name);
        if !dep_name.ends_with(".text") {
            return None;
        }
        let effective_name = if let Some(prefix) = &self.scope_prefix {
            let first = dep_name.split('.').next().unwrap_or(dep_name);
            let first_is_known = self
                .compiler
                .variables
                .iter()
                .any(|(name, _)| name == first || name.starts_with(&format!("{first}.")));
            if dep_name.starts_with(&format!("{prefix}."))
                || dep_name.starts_with("store.")
                || dep_name.starts_with("theme_options.")
                || first_is_known
            {
                dep_name.to_string()
            } else {
                format!("{prefix}.{dep_name}")
            }
        } else {
            dep_name.to_string()
        };
        if self.reactive_vars.contains_key(&effective_name) {
            return Some(effective_name);
        }
        let link_base = effective_name.strip_suffix(".text")?;
        let change_path = format!("{}.event.change", link_base);
        let input_id = self.add_input(InputKind::TextChange, Some(change_path));
        let input_var = self.fresh_var("text_state_input");
        self.collections
            .insert(input_var.clone(), CollectionSpec::Input(input_id));

        let default_var = self.fresh_var("text_state_default");
        self.collections.insert(
            default_var.clone(),
            CollectionSpec::Literal(Value::text("")),
        );

        let state_var = VarId::new(effective_name.as_str());
        self.collections.insert(
            state_var.clone(),
            CollectionSpec::HoldLatest(vec![default_var, input_var]),
        );
        self.reactive_vars
            .insert(effective_name.clone(), state_var.clone());
        Some(effective_name)
    }

    /// Inline-compile a `List/map(item, new: expr)` on a keyed source.
    ///
    /// Produces a `CollectionSpec::ListMap` that transforms each keyed item.
    fn compile_inline_keyed_map(
        &mut self,
        keyed_var: VarId,
        arguments: &[Spanned<Argument>],
    ) -> Result<VarId, String> {
        let item_param: String = arguments
            .iter()
            .find(|a| a.node.value.is_none())
            .map(|a| a.node.name.as_str().to_string())
            .unwrap_or_else(|| "item".to_string());

        let new_expr = arguments
            .iter()
            .find(|a| a.node.name.as_str() == "new")
            .and_then(|a| a.node.value.as_ref())
            .ok_or_else(|| "List/map missing 'new' argument".to_string())?;

        let compiler = self.compiler.clone();
        let new_expr = new_expr.clone();
        let param_name = item_param;

        let map_var = self.fresh_var("keyed_map");
        self.collections.insert(
            map_var.clone(),
            CollectionSpec::ListMap {
                source: keyed_var,
                f: Arc::new(move |item: &Value| {
                    let mut scope = indexmap::IndexMap::new();
                    scope.insert(param_name.clone(), item.clone());
                    compiler.eval_static_tolerant(&new_expr, &scope)
                }),
            },
        );
        self.keyed_collection_vars.insert(map_var.clone());
        Ok(map_var)
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
        let pipeline = self
            .find_display_pipeline_in_functions(list_name)
            .ok_or_else(|| format!("No display pipeline found for '{}'", list_name))?;

        // Build keyed retain (if present) or use raw keyed var
        let map_source = if let Some(ref retain_args) = pipeline.retain_arguments {
            self.compile_inline_keyed_retain(
                keyed_var.clone(),
                retain_args,
                &pipeline.scope_bindings,
            )?
        } else {
            keyed_var.clone()
        };

        // Build ListMapWithKey (transform items to element Values + inject link paths)
        let display_var = self.fresh_var("display_pipeline");
        let compiler = self.compiler.clone();
        let map_new_expr = pipeline.map_new_expr.clone();
        let map_item_param = pipeline.map_item_param.clone();
        let scope_bindings = pipeline.scope_bindings.clone();
        let initial_passed = self.initial_passed.clone();
        let list_path = format!(
            "store.{}",
            list_name.strip_prefix("store.").unwrap_or(list_name)
        );
        let mut reactive_deps = self.find_reactive_deps_in_expr(&pipeline.map_new_expr);
        for (_, binding_expr) in &pipeline.scope_bindings {
            for dep in self.find_reactive_deps_in_expr(binding_expr) {
                if !reactive_deps.contains(&dep) {
                    reactive_deps.push(dep);
                }
            }
        }

        if reactive_deps.is_empty() {
            self.collections.insert(
                display_var.clone(),
                CollectionSpec::ListMapWithKey {
                    source: map_source,
                    f: Arc::new(move |key: &ListKey, item: &Value| {
                        let item_with_links =
                            inject_item_link_paths_with_key(item, &list_path, key.0.as_ref());
                        let mut scope = IndexMap::new();
                        scope.insert(map_item_param.clone(), item_with_links);
                        if !matches!(&initial_passed, Value::Unit) {
                            scope.insert(PASSED_VAR.to_string(), initial_passed.clone());
                        }
                        compiler.populate_scope_bindings_tolerant(&mut scope, &scope_bindings);
                        compiler.eval_static_tolerant(&map_new_expr, &scope)
                    }),
                },
            );
        } else {
            let real_dep_names: Vec<String> = reactive_deps
                .iter()
                .filter(|name| {
                    if name.contains(".__state") {
                        return false;
                    }
                    if let Some(var_id) = self.reactive_vars.get(*name) {
                        if let Some(spec) = self.collections.get(var_id) {
                            if matches!(spec, CollectionSpec::SideEffect { .. }) {
                                return false;
                            }
                        }
                    }
                    true
                })
                .cloned()
                .collect();
            if real_dep_names.is_empty() {
                return Err("List/map display: no real reactive deps found".to_string());
            }

            let dep_var = if real_dep_names.len() == 1 {
                let reactive_dep = real_dep_names[0].clone();
                let reactive_var =
                    self.reactive_vars
                        .get(&reactive_dep)
                        .cloned()
                        .ok_or_else(|| {
                            format!(
                                "List/map display: reactive dep '{}' not found",
                                reactive_dep
                            )
                        })?;
                if self.has_initial_value(&reactive_var) {
                    reactive_var
                } else {
                    let dep_default = self.fresh_var("display_dep_default");
                    self.collections.insert(
                        dep_default.clone(),
                        CollectionSpec::Literal(Value::Object(Arc::new(BTreeMap::from([(
                            Arc::from(format!("{DEP_FIELD_PREFIX}0")),
                            Value::Unit,
                        )])))),
                    );
                    let dep_wrapped = self.fresh_var("display_dep_wrapped");
                    self.collections.insert(
                        dep_wrapped.clone(),
                        CollectionSpec::Map {
                            source: reactive_var,
                            f: Arc::new(move |value: &Value| {
                                Value::Object(Arc::new(BTreeMap::from([(
                                    Arc::from(format!("{DEP_FIELD_PREFIX}0")),
                                    value.clone(),
                                )])))
                            }),
                        },
                    );
                    let dep_with_default = self.fresh_var("display_dep_with_default");
                    self.collections.insert(
                        dep_with_default.clone(),
                        CollectionSpec::HoldLatest(vec![dep_default, dep_wrapped]),
                    );
                    dep_with_default
                }
            } else {
                let first_var = self
                    .reactive_vars
                    .get(&real_dep_names[0])
                    .cloned()
                    .ok_or_else(|| {
                        format!(
                            "List/map display: reactive dep '{}' not found",
                            real_dep_names[0]
                        )
                    })?;
                let second_var = self
                    .reactive_vars
                    .get(&real_dep_names[1])
                    .cloned()
                    .ok_or_else(|| {
                        format!(
                            "List/map display: reactive dep '{}' not found",
                            real_dep_names[1]
                        )
                    })?;
                let mut current_var = self.fresh_var("display_joined_deps");
                self.collections.insert(
                    current_var.clone(),
                    CollectionSpec::Join {
                        left: first_var,
                        right: second_var,
                        combine: Arc::new(move |a: &Value, b: &Value| {
                            Value::object([
                                (format!("{DEP_FIELD_PREFIX}0"), a.clone()),
                                (format!("{DEP_FIELD_PREFIX}1"), b.clone()),
                            ])
                        }),
                    },
                );

                for (i, dep_name) in real_dep_names.iter().enumerate().skip(2) {
                    let dep_var = self.reactive_vars.get(dep_name).cloned().ok_or_else(|| {
                        format!("List/map display: reactive dep '{}' not found", dep_name)
                    })?;
                    let prev = current_var.clone();
                    current_var = self.fresh_var("display_joined_deps");
                    let idx = i;
                    self.collections.insert(
                        current_var.clone(),
                        CollectionSpec::Join {
                            left: prev,
                            right: dep_var,
                            combine: Arc::new(move |combined: &Value, new_dep: &Value| {
                                let key = format!("{DEP_FIELD_PREFIX}{idx}");
                                combined.update_field(&key, new_dep.clone())
                            }),
                        },
                    );
                }

                current_var
            };
            let dep_names = real_dep_names.clone();

            self.collections.insert(
                display_var.clone(),
                CollectionSpec::ListMapWithKeyReactive {
                    source: map_source,
                    dep: dep_var,
                    f: Arc::new(move |key: &ListKey, item: &Value, dep: &Value| {
                        let item_with_links =
                            inject_item_link_paths_with_key(item, &list_path, key.0.as_ref());
                        let mut scope = IndexMap::new();
                        scope.insert(map_item_param.clone(), item_with_links);

                        let mut passed_val = if matches!(&initial_passed, Value::Unit) {
                            Value::Object(Arc::new(BTreeMap::new()))
                        } else {
                            initial_passed.clone()
                        };
                        for (idx, dep_name) in dep_names.iter().enumerate() {
                            let dep_value = dep
                                .get_field(&format!("{DEP_FIELD_PREFIX}{idx}"))
                                .cloned()
                                .unwrap_or(Value::Unit);
                            inject_scope_path(&mut scope, dep_name, dep_value.clone());
                            passed_val = set_nested_field(&passed_val, dep_name, dep_value);
                        }
                        scope.insert(PASSED_VAR.to_string(), passed_val);

                        compiler.populate_scope_bindings_tolerant(&mut scope, &scope_bindings);
                        compiler.eval_static_tolerant(&map_new_expr, &scope)
                    }),
                },
            );
        }
        self.keyed_collection_vars.insert(display_var.clone());
        Ok(display_var)
    }

    /// Extract the initial PASSED context from the document expression.
    ///
    /// Follows the doc expression (which may be an alias like `scene` pointing
    /// to a function call like `main_scene(PASS: [store: store, theme_options: theme_options])`)
    /// and evaluates the PASS argument tolerantly to capture the initial program state.
    fn extract_initial_passed(&self, doc_expr: &Spanned<Expression>) -> Value {
        // Resolve the doc expression — it may be an alias pointing to a function call
        let resolved = match &doc_expr.node {
            Expression::Alias(Alias::WithoutPassed { parts, .. }) if parts.len() == 1 => {
                self.compiler.get_var_expr(parts[0].as_str()).cloned()
            }
            _ => Some(doc_expr.clone()),
        };
        let resolved = match resolved {
            Some(expr) => expr,
            None => return Value::Unit,
        };
        // Extract the PASS argument from the function call
        let pass_expr = match &resolved.node {
            Expression::FunctionCall { arguments, .. } => arguments.iter().find_map(|arg| {
                if arg.node.name.as_str() == "PASS" {
                    arg.node.value.as_ref().cloned()
                } else {
                    None
                }
            }),
            _ => None,
        };
        match pass_expr {
            Some(expr) => self.compiler.eval_static_tolerant(&expr, &IndexMap::new()),
            None => Value::Unit,
        }
    }

    /// Scan function bodies for the display pipeline pattern on a keyed list.
    ///
    /// Looks for: `PASSED.store.<list_name> |> List/retain(...) |> List/map(item, new: ...)`
    /// in `items:` arguments of Element/stripe calls within function bodies.
    fn find_display_pipeline_in_functions(&self, list_name: &str) -> Option<DisplayPipelineInfo> {
        // The keyed name without "store." prefix for matching PASSED.store.<name>
        let short_name = list_name.strip_prefix("store.").unwrap_or(list_name);

        for (_fn_name, _params, body) in self.compiler.functions.iter() {
            if let Some(info) = self.find_display_pipeline_in_expr(body, short_name, &[]) {
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
        scope_bindings: &[(String, Spanned<Expression>)],
    ) -> Option<DisplayPipelineInfo> {
        match &expr.node {
            // Check Element/stripe calls for items: argument with the pattern
            Expression::FunctionCall { path, arguments } => {
                let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                if matches!(
                    path_strs.as_slice(),
                    ["Element", "stripe"] | ["Scene", "Element", "stripe"]
                ) {
                    // Check the items: argument
                    if let Some(items_arg) =
                        arguments.iter().find(|a| a.node.name.as_str() == "items")
                    {
                        if let Some(ref items_expr) = items_arg.node.value {
                            if let Some(info) = self.match_display_pipeline_pattern(
                                items_expr,
                                list_short_name,
                                scope_bindings,
                            ) {
                                return Some(info);
                            }
                        }
                    }
                }
                // Recurse into all argument values
                for arg in arguments {
                    if let Some(ref val_expr) = arg.node.value {
                        if let Some(info) = self.find_display_pipeline_in_expr(
                            val_expr,
                            list_short_name,
                            scope_bindings,
                        ) {
                            return Some(info);
                        }
                    }
                }
            }
            Expression::Pipe { from, to } => {
                if let Some(info) =
                    self.find_display_pipeline_in_expr(from, list_short_name, scope_bindings)
                {
                    return Some(info);
                }
                if let Some(info) =
                    self.find_display_pipeline_in_expr(to, list_short_name, scope_bindings)
                {
                    return Some(info);
                }
            }
            Expression::Block {
                variables, output, ..
            } => {
                let mut nested_bindings = scope_bindings.to_vec();
                for var in variables {
                    if let Some(info) = self.find_display_pipeline_in_expr(
                        &var.node.value,
                        list_short_name,
                        &nested_bindings,
                    ) {
                        return Some(info);
                    }
                    nested_bindings
                        .push((var.node.name.as_str().to_string(), var.node.value.clone()));
                }
                if let Some(info) =
                    self.find_display_pipeline_in_expr(output, list_short_name, &nested_bindings)
                {
                    return Some(info);
                }
            }
            Expression::While { arms } | Expression::When { arms } => {
                for arm in arms {
                    if let Some(info) = self.find_display_pipeline_in_expr(
                        &arm.body,
                        list_short_name,
                        scope_bindings,
                    ) {
                        return Some(info);
                    }
                }
            }
            Expression::List { items } => {
                for item in items {
                    if let Some(info) =
                        self.find_display_pipeline_in_expr(item, list_short_name, scope_bindings)
                    {
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
        scope_bindings: &[(String, Spanned<Expression>)],
    ) -> Option<DisplayPipelineInfo> {
        if let Expression::Pipe {
            from: outer_from,
            to: outer_to,
        } = &expr.node
        {
            // Check outer_to is List/map(...)
            if let Expression::FunctionCall {
                path: map_path,
                arguments: map_args,
            } = &outer_to.node
            {
                let map_path_strs: Vec<&str> = map_path.iter().map(|s| s.as_str()).collect();
                if map_path_strs.as_slice() != ["List", "map"] {
                    return None;
                }

                // Extract map info (shared by both patterns)
                let map_item_param = map_args
                    .iter()
                    .find(|a| a.node.value.is_none())
                    .map(|a| a.node.name.as_str().to_string())
                    .unwrap_or_else(|| "item".to_string());

                let map_new_expr = map_args
                    .iter()
                    .find(|a| a.node.name.as_str() == "new")
                    .and_then(|a| a.node.value.as_ref())?
                    .clone();

                // Pattern 1: PASSED.store.<name> |> List/retain(...) |> List/map(...)
                if let Expression::Pipe {
                    from: inner_from,
                    to: inner_to,
                } = &outer_from.node
                {
                    if self.is_passed_store_alias(inner_from, list_short_name) {
                        if let Expression::FunctionCall {
                            path: retain_path,
                            arguments: retain_args,
                        } = &inner_to.node
                        {
                            let retain_path_strs: Vec<&str> =
                                retain_path.iter().map(|s| s.as_str()).collect();
                            if retain_path_strs.as_slice() == ["List", "retain"] {
                                return Some(DisplayPipelineInfo {
                                    retain_arguments: Some(retain_args.clone()),
                                    map_item_param,
                                    map_new_expr,
                                    scope_bindings: scope_bindings.to_vec(),
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
                        scope_bindings: scope_bindings.to_vec(),
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
        // Check if this is part of a list chain (e.g., LIST {} |> List/append(...) |> List/retain(...))
        // If the predicate references per-item data ("item"), route through the keyed holdstate path
        // so that downstream operators (List/latest, List/every, etc.) get keyed collections.
        let predicate_has_item = arguments
            .iter()
            .find(|a| a.node.name.as_str() == "if")
            .and_then(|a| a.node.value.as_ref())
            .map(|e| Self::expr_references_name(e, "item"))
            .unwrap_or(false);

        if predicate_has_item {
            // Try to walk the pipe chain backward to find a list initializer
            let mut ops = Vec::new();
            ops.push(ListChainOp::Retain(arguments));
            if let Ok(initial_list_expr) = self.collect_list_chain_ops(from, &mut ops) {
                return self.build_unified_list_holdstate(name, initial_list_expr, &ops);
            }
        }

        // Extract the item parameter name (e.g., "n" from `List/retain(n, if: ...)`)
        let item_param: String = arguments
            .iter()
            .find(|a| a.node.value.is_none())
            .map(|a| a.node.name.as_str().to_string())
            .unwrap_or_else(|| "item".to_string());

        // Extract the `if:` predicate expression
        let predicate_expr = arguments
            .iter()
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
                            let result = compiler
                                .eval_static_with_scope(&pred_expr, &scope)
                                .unwrap_or(Value::Unit);
                            result.as_bool().unwrap_or(false)
                        })
                    }),
                },
            );
            self.reactive_vars
                .insert(name.to_string(), retain_var.clone());
            Ok(retain_var)
        } else {
            // Reactive predicate: Join source list with reactive deps, then filter
            // For simplicity, support a single reactive dependency for now
            let reactive_dep = reactive_deps[0].clone();
            let reactive_var = self
                .reactive_vars
                .get(&reactive_dep)
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
                            let result = compiler
                                .eval_static_with_scope(&pred_expr, &scope)
                                .unwrap_or(Value::Unit);
                            result.as_bool().unwrap_or(false)
                        })
                    }),
                },
            );
            self.reactive_vars
                .insert(name.to_string(), retain_var.clone());
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
        let item_param: String = arguments
            .iter()
            .find(|a| a.node.value.is_none())
            .map(|a| a.node.name.as_str().to_string())
            .unwrap_or_else(|| "item".to_string());

        let new_expr = arguments
            .iter()
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
                        compiler
                            .eval_static_with_scope(&new_expr, &scope)
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
            Expression::Pipe { from, to } => match &to.node {
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
                        ["List", "remove_last"] => {
                            ops.push(ListChainOp::RemoveLast(arguments));
                            self.collect_list_chain_ops(from, ops)
                        }
                        ["List", "clear"] => {
                            ops.push(ListChainOp::Clear(arguments));
                            self.collect_list_chain_ops(from, ops)
                        }
                        ["List", "retain"] => {
                            ops.push(ListChainOp::Retain(arguments));
                            self.collect_list_chain_ops(from, ops)
                        }
                        _ => Ok(expr),
                    }
                }
                _ => Ok(expr),
            },
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
        // Pre-check: if any Remove/Retain op references "item", use keyed pipeline
        let has_wildcard_ops = ops.iter().rev().any(|op| match op {
            ListChainOp::Remove(arguments) => {
                let on_arg = arguments
                    .iter()
                    .find(|a| a.node.name.as_str() == "on")
                    .and_then(|a| a.node.value.as_ref());
                if let Some(on_expr) = on_arg {
                    return Self::expr_references_name(on_expr, "item");
                }
                false
            }
            ListChainOp::Retain(arguments) => {
                let if_arg = arguments
                    .iter()
                    .find(|a| a.node.name.as_str() == "if")
                    .and_then(|a| a.node.value.as_ref());
                if let Some(if_expr) = if_arg {
                    return Self::expr_references_name(if_expr, "item");
                }
                false
            }
            _ => false,
        });
        if has_wildcard_ops {
            return self.build_keyed_list_holdstate(name, initial_list_expr, ops);
        }

        let mut event_vars = Vec::new();
        let mut remove_counter = 0u32;

        // Evaluate initial list statically
        let initial_list = if let Expression::List { items } = &initial_list_expr.node {
            let mut fields = BTreeMap::new();
            for (i, item) in items.iter().enumerate() {
                let val = self.compiler.eval_static_tolerant(item, &IndexMap::new());
                fields.insert(Arc::from(format!("{:04}", i)), val);
            }
            Value::Tagged {
                tag: Arc::from(LIST_TAG),
                fields: Arc::new(fields),
            }
        } else {
            self.compiler
                .eval_static_tolerant(initial_list_expr, &IndexMap::new())
        };

        // Process each operation (they were collected outermost-first, so reverse to get pipeline order)
        for op in ops.iter().rev() {
            match op {
                ListChainOp::Append(arguments) => {
                    // Accept both `item:` and `on:` — `on:` is event-triggered append
                    let item_arg = arguments
                        .iter()
                        .find(|a| a.node.name.as_str() == "item" || a.node.name.as_str() == "on")
                        .and_then(|a| a.node.value.as_ref())
                        .ok_or_else(|| "List/append missing 'item' or 'on' argument".to_string())?;
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
                    let on_arg = arguments
                        .iter()
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

                ListChainOp::RemoveLast(arguments) => {
                    let on_arg = arguments
                        .iter()
                        .find(|a| a.node.name.as_str() == "on")
                        .and_then(|a| a.node.value.as_ref())
                        .ok_or_else(|| "List/remove_last missing 'on' argument".to_string())?;

                    let (event_var, _) = self.compile_event_source(on_arg)?;
                    let tagged_remove_last = self.fresh_var("tagged_remove_last");
                    self.collections.insert(
                        tagged_remove_last.clone(),
                        CollectionSpec::Then {
                            source: event_var,
                            body: Arc::new(|_v: &Value| {
                                Value::object([("__t", Value::text("remove_last"))])
                            }),
                        },
                    );
                    event_vars.push(tagged_remove_last);
                }

                ListChainOp::Clear(arguments) => {
                    let clear_source_expr = arguments
                        .iter()
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

                ListChainOp::Retain(_) => {
                    // Retain with per-item predicates is handled by build_keyed_list_holdstate.
                    // This arm should not be reached in the scalar path.
                }
            }
        }

        // Concat all events
        let events_var = if event_vars.len() == 1 {
            event_vars.into_iter().next().unwrap()
        } else if event_vars.is_empty() {
            let empty_var = self.fresh_var("no_events");
            self.collections
                .insert(empty_var.clone(), CollectionSpec::Literal(Value::Unit));
            empty_var
        } else {
            let concat_var = self.fresh_var("list_events");
            self.collections
                .insert(concat_var.clone(), CollectionSpec::Concat(event_vars));
            concat_var
        };

        // HoldState: maintains the items list
        let initial_var = self.fresh_var("list_init");
        let effective_initial = self
            .persisted_holds
            .get(name)
            .cloned()
            .unwrap_or_else(|| initial_list.clone());
        self.collections.insert(
            initial_var.clone(),
            CollectionSpec::Literal(effective_initial.clone()),
        );

        // Build transform that handles all event types
        let list_var = VarId::new(name);
        self.collections.insert(
            list_var.clone(),
            CollectionSpec::HoldState {
                initial: initial_var,
                events: events_var,
                initial_value: effective_initial,
                transform: Arc::new(|state: &Value, event: &Value| {
                    let event_type = event
                        .get_field("__t")
                        .and_then(|v| v.as_text())
                        .unwrap_or("")
                        .to_string();
                    match event_type.as_str() {
                        "append" => {
                            let item = event.get_field("v").cloned().unwrap_or(Value::Unit);
                            let count = state.list_count();
                            state.list_append(item, count)
                        }
                        "remove_last" => state.list_remove_last(),
                        "clear" => Value::empty_list(),
                        _ => state.clone(),
                    }
                }),
            },
        );

        self.reactive_vars
            .insert(name.to_string(), list_var.clone());

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
    /// LiteralList → AppendNewKeyed → ListAppend → KeyedHoldState
    /// with MapToKeyed for wildcard event demuxing and per-item transform.
    fn build_keyed_list_holdstate(
        &mut self,
        name: &str,
        initial_list_expr: &Spanned<Expression>,
        ops: &[ListChainOp],
    ) -> Result<VarId, String> {
        use super::types::ClassifyFn;

        // Evaluate initial list statically.
        // For keyed CRUD-style lists, evaluate items one by one so we can
        // tolerate reactive fields inside constructor calls without hanging
        // the whole list build on a single recursive path.
        let initial_list = if let Expression::List { items } = &initial_list_expr.node {
            let mut fields = BTreeMap::new();
            for (i, item) in items.iter().enumerate() {
                let val = self.compiler.eval_static_tolerant(item, &IndexMap::new());
                fields.insert(Arc::from(format!("{:04}", i)), val);
            }
            Value::Tagged {
                tag: Arc::from(LIST_TAG),
                fields: Arc::new(fields),
            }
        } else {
            self.compiler
                .eval_static_tolerant(initial_list_expr, &IndexMap::new())
        };

        // Get effective initial (with persistence)
        let effective_initial = self
            .persisted_holds
            .get(name)
            .cloned()
            .unwrap_or_else(|| initial_list.clone());

        // Decompose initial list into (ListKey, Value) pairs
        let initial_items: Vec<(ListKey, Value)> = if let Value::Tagged {
            ref tag,
            ref fields,
        } = effective_initial
        {
            if tag.as_ref() == LIST_TAG {
                fields
                    .iter()
                    .map(|(k, v)| (ListKey::new(k.as_ref()), v.clone()))
                    .collect()
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        let initial_counter = initial_items.len();

        // 1. LiteralList for initial items
        let initial_list_var = self.fresh_var("keyed_initial");
        self.collections.insert(
            initial_list_var.clone(),
            CollectionSpec::LiteralList(initial_items),
        );

        // 2. Find append source from ops and create AppendNewKeyed
        let mut append_source: Option<VarId> = None;
        for op in ops.iter().rev() {
            if let ListChainOp::Append(arguments) = op {
                let item_arg = arguments
                    .iter()
                    .find(|a| a.node.name.as_str() == "item" || a.node.name.as_str() == "on")
                    .and_then(|a| a.node.value.as_ref())
                    .ok_or_else(|| "List/append missing 'item' or 'on' argument".to_string())?;
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
        self.collections.insert(
            wildcard_input_var.clone(),
            CollectionSpec::Input(wildcard_id),
        );

        let classify: ClassifyFn = Arc::new(|v: &Value| {
            let path = v.get_field("path")?.as_text()?.to_string();
            let event_value = v.get_field("value").cloned().unwrap_or(Value::Unit);

            let key = extract_list_item_key_from_path(&path)?;

            // Find subpath after the key
            let subpath = path
                .split('.')
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
                if subpath.contains(".event.key_down") {
                    let key = match event_value.as_text() {
                        Some(key) => key,
                        None => return None,
                    };
                    match key {
                        "Enter" => Value::object([("type", Value::text("edit_key_enter"))]),
                        "Escape" => Value::object([("type", Value::text("edit_key_escape"))]),
                        _ => return None,
                    }
                } else if subpath.contains(".event.change") {
                    let text = match event_value.as_text() {
                        Some(text) => text,
                        None => return None,
                    };
                    Value::object([
                        ("type", Value::text("edit_text_change")),
                        ("text", Value::text(text)),
                    ])
                } else if subpath.contains(".event.blur") && event_tag == "Blur" {
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

        let keyed_transform: HoldTransformFn = Arc::new(|state: &Value, event: &Value| {
            let event_type = event
                .get_field("type")
                .and_then(|v| v.as_text())
                .unwrap_or("")
                .to_string();

            match event_type.as_str() {
                "remove" => Value::Unit,
                "checkbox_click" => {
                    let completed = state
                        .get_field("completed")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    state.update_field(
                        "completed",
                        if !completed {
                            Value::tag("True")
                        } else {
                            Value::tag("False")
                        },
                    )
                }
                "double_click" => {
                    let title = state
                        .get_field("title")
                        .and_then(|v| v.as_text())
                        .unwrap_or("")
                        .to_string();
                    state
                        .update_field("editing", Value::tag("True"))
                        .update_field("__edit_text", Value::text(title.as_str()))
                }
                "edit_key_enter" => {
                    let edit_text = state
                        .get_field("__edit_text")
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
                "edit_key_escape" => state.update_field("editing", Value::tag("False")),
                "edit_text_change" => {
                    let text = event
                        .get_field("text")
                        .and_then(|v| v.as_text())
                        .unwrap_or("");
                    state.update_field("__edit_text", Value::text(text))
                }
                "edit_blur" => {
                    let edit_text = state
                        .get_field("__edit_text")
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
                    let hovered = event
                        .get_field("hovered")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    state.update_field(
                        HOVERED_FIELD,
                        if hovered {
                            Value::tag("True")
                        } else {
                            Value::tag("False")
                        },
                    )
                }
                _ => state.clone(),
            }
        });

        // Register the keyed membership stream early so forward dependents like
        // `store.selected_id` can compile against stable per-item identity
        // without introducing a cycle through the final keyed hold state.
        self.keyed_hold_vars
            .insert(name.to_string(), membership_var.clone());
        self.keyed_collection_vars.insert(membership_var.clone());

        // 5. Detect broadcast sources (toggle_all, remove_completed)
        let mut broadcast_vars: Vec<VarId> = Vec::new();
        let mut has_broadcasts = false;
        let mut custom_broadcast_handlers: Vec<BroadcastHandlerFn> = Vec::new();

        // Toggle-all: check if the store has a toggle_all_checkbox LINK
        if let Some(toggle_expr) = self
            .compiler
            .get_var_expr("store.elements.toggle_all_checkbox")
        {
            if matches!(toggle_expr.node, Expression::Link) {
                let toggle_path = "store.elements.toggle_all_checkbox".to_string();
                let input_id = self.add_input(InputKind::LinkClick, Some(toggle_path));
                let toggle_input_var = self.fresh_var("toggle_all_input");
                self.collections
                    .insert(toggle_input_var.clone(), CollectionSpec::Input(input_id));
                // THEN: produce a tagged broadcast event
                let toggle_then_var = self.fresh_var("toggle_all_then");
                self.collections.insert(
                    toggle_then_var.clone(),
                    CollectionSpec::Then {
                        source: toggle_input_var,
                        body: Arc::new(|_| Value::tag("toggle_all")),
                    },
                );
                broadcast_vars.push(toggle_then_var);
                has_broadcasts = true;
            }
        }

        // Remove-completed: detect from Remove ops
        for op in ops.iter().rev() {
            if let ListChainOp::Remove(arguments) = op {
                let on_arg = arguments
                    .iter()
                    .find(|a| a.node.name.as_str() == "on")
                    .and_then(|a| a.node.value.as_ref());
                if let Some(on_expr) = on_arg {
                    if Self::expr_references_name(on_expr, "item") {
                        if let Expression::Pipe { from, to } = &on_expr.node {
                            if let Expression::Then { body } = &to.node {
                                if !Self::expr_references_name(from, "item") {
                                    let (event_var, _) = self.compile_event_source(from)?;
                                    let reactive_deps = self.find_reactive_deps_in_expr(body);
                                    if reactive_deps.is_empty() {
                                        let remove_then_var =
                                            self.fresh_var("remove_completed_then");
                                        self.collections.insert(
                                            remove_then_var.clone(),
                                            CollectionSpec::Then {
                                                source: event_var,
                                                body: Arc::new(|_| Value::tag("remove_completed")),
                                            },
                                        );
                                        broadcast_vars.push(remove_then_var);
                                    } else {
                                        let reactive_dep = reactive_deps[0].clone();
                                        let dep_var = if let Some(dep_var) =
                                            self.reactive_vars.get(&reactive_dep).cloned()
                                        {
                                            dep_var
                                        } else if let Some(dep_expr) =
                                            self.compiler.get_var_expr(&reactive_dep).cloned()
                                        {
                                            self.compile_reactive_var(&reactive_dep, &dep_expr)?
                                        } else {
                                            return Err(format!(
                                                "Keyed remove: reactive dep '{}' not found",
                                                reactive_dep
                                            ));
                                        };
                                        let remove_tag = format!(
                                            "broadcast_remove_{}",
                                            custom_broadcast_handlers.len()
                                        );
                                        let tag_clone = remove_tag.clone();
                                        let sample_var = self.fresh_var("remove_broadcast_sample");
                                        self.collections.insert(
                                            sample_var.clone(),
                                            CollectionSpec::SampleOnEvent {
                                                event: event_var,
                                                dep: dep_var,
                                                f: Arc::new(move |_event: &Value, dep: &Value| {
                                                    let mut fields = BTreeMap::new();
                                                    fields.insert(
                                                        Arc::from("__t"),
                                                        Value::text(tag_clone.as_str()),
                                                    );
                                                    fields.insert(
                                                        Arc::from(format!("{DEP_FIELD_PREFIX}0")),
                                                        dep.clone(),
                                                    );
                                                    Value::Object(Arc::new(fields))
                                                }),
                                            },
                                        );
                                        broadcast_vars.push(sample_var);

                                        let compiler = self.compiler.clone();
                                        let body_expr = body.as_ref().clone();
                                        let dep_name = reactive_dep.clone();
                                        let handler_tag = remove_tag.clone();
                                        custom_broadcast_handlers.push(Arc::new(
                                            move |states, event| {
                                                if event
                                                    .get_field("__t")
                                                    .and_then(|v| v.as_text())
                                                    != Some(handler_tag.as_str())
                                                {
                                                    return Vec::new();
                                                }
                                                        let dep_value = event
                                                            .get_field(&format!("{DEP_FIELD_PREFIX}0"))
                                                            .cloned()
                                                            .unwrap_or(Value::Unit);
                                                states
                                                    .iter()
                                                    .filter_map(|(key, item)| {
                                                        let mut scope = IndexMap::new();
                                                        scope.insert("item".to_string(), item.clone());
                                                        inject_scope_path(
                                                            &mut scope,
                                                            &dep_name,
                                                            dep_value.clone(),
                                                        );
                                                        let mut passed =
                                                            Value::Object(Arc::new(BTreeMap::new()));
                                                        passed = set_nested_field(
                                                            &passed,
                                                            &dep_name,
                                                            dep_value.clone(),
                                                        );
                                                        scope.insert(
                                                            PASSED_VAR.to_string(),
                                                            passed,
                                                        );
                                                        let result = compiler
                                                            .eval_static_tolerant(&body_expr, &scope);
                                                        #[cfg(not(target_arch = "wasm32"))]
                                                        std::eprintln!(
                                                            "[dd-broadcast-remove] dep_name={} key={} dep={} item={} result={}",
                                                            dep_name,
                                                            key,
                                                            dep_value,
                                                            item,
                                                            result
                                                        );
                                                        if matches!(result, Value::Unit) {
                                                            None
                                                        } else {
                                                            Some((key.clone(), None))
                                                        }
                                                    })
                                                    .collect()
                                            },
                                        ));
                                    }
                                    has_broadcasts = true;
                                }
                            }
                        }
                    }
                }
            }
        }

        // Remove-completed: detect from Retain ops (LATEST predicate arms)
        //
        // A Retain predicate like:
        //   List/retain(item, if: LATEST {
        //       True
        //       item.remove_button.event.press |> THEN { False }           ← per-item (wildcard)
        //       global_button.event.press |> THEN { item.completed |> ... } ← broadcast
        //   })
        //
        // The third arm is a broadcast: a global event that conditionally removes items.
        // We detect it the same way as Remove: Pipe(from, Then { ... }) where `from`
        // doesn't reference "item" but the whole arm does.
        for op in ops.iter().rev() {
            if let ListChainOp::Retain(arguments) = op {
                let if_arg = arguments
                    .iter()
                    .find(|a| a.node.name.as_str() == "if")
                    .and_then(|a| a.node.value.as_ref());
                if let Some(if_expr) = if_arg {
                    // If the predicate is a LATEST block, check each arm
                    if let Expression::Latest { inputs } = &if_expr.node {
                        for arm in inputs {
                            if let Expression::Pipe { from, to } = &arm.node {
                                if matches!(&to.node, Expression::Then { .. }) {
                                    if Self::expr_references_name(arm, "item")
                                        && !Self::expr_references_name(from, "item")
                                    {
                                        let (event_var, _) = self.compile_event_source(from)?;
                                        let remove_then_var =
                                            self.fresh_var("remove_completed_then");
                                        self.collections.insert(
                                            remove_then_var.clone(),
                                            CollectionSpec::Then {
                                                source: event_var,
                                                body: Arc::new(|_| Value::tag("remove_completed")),
                                            },
                                        );
                                        broadcast_vars.push(remove_then_var);
                                        has_broadcasts = true;
                                    }
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
                self.collections
                    .insert(concat_var.clone(), CollectionSpec::Concat(broadcast_vars));
                Some(concat_var)
            }
        } else {
            None
        };

        // Build broadcast handler
        let broadcast_handler: Option<BroadcastHandlerFn> = if has_broadcasts {
            let custom_handlers = custom_broadcast_handlers.clone();
            Some(Arc::new(
                move |states: &std::collections::HashMap<ListKey, Value>, event: &Value| {
                    let event_tag = event.as_tag().unwrap_or("");
                    let mut results = match event_tag {
                        "toggle_all" => {
                            let all_completed = !states.is_empty()
                                && states.values().all(|item| {
                                    item.get_field("completed")
                                        .and_then(|v| v.as_bool())
                                        .unwrap_or(false)
                                });
                            let target = !all_completed;
                            let target_val = if target {
                                Value::tag("True")
                            } else {
                                Value::tag("False")
                            };
                            states
                                .iter()
                                .map(|(key, item)| {
                                    let new_item =
                                        item.update_field("completed", target_val.clone());
                                    (key.clone(), Some(new_item))
                                })
                                .collect()
                        }
                        "remove_completed" => states
                            .iter()
                            .filter_map(|(key, item)| {
                                let completed = item
                                    .get_field("completed")
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(false);
                                if completed {
                                    Some((key.clone(), None))
                                } else {
                                    None
                                }
                            })
                            .collect(),
                        _ => Vec::new(),
                    };
                    for handler in &custom_handlers {
                        results.extend(handler(states, event));
                    }
                    results
                },
            ))
        } else {
            None
        };

        // 6. KeyedHoldState: per-item state with self-removal sentinel + broadcast support
        let keyed_hold_var = self.fresh_var("keyed_hold");
        self.keyed_hold_vars
            .insert(name.to_string(), keyed_hold_var.clone());
        self.keyed_collection_vars.insert(keyed_hold_var.clone());
        self.collections.insert(
            keyed_hold_var.clone(),
            CollectionSpec::KeyedHoldState {
                initial: membership_var,
                events: wildcard_keyed_var,
                transform: keyed_transform,
                broadcasts: broadcasts_var,
                broadcast_handler,
            },
        );

        // 7. Assembled list for document closure.
        // Items render via keyed diffs from the display pipeline — O(1) per item change.
        // The document closure also needs the assembled list for aggregation operations
        // (List/count, List/every, List/any, List/retain |> List/count) that access
        // per-item fields (e.g., item.completed). ListAssemble converts the keyed
        // collection into a scalar List value with real per-item data.
        // HoldLatest ensures an initial empty list value even before any items arrive.
        let assemble_raw_var = self.fresh_var(&format!("{}_assemble_raw", name));
        self.collections.insert(
            assemble_raw_var.clone(),
            CollectionSpec::ListAssemble(keyed_hold_var),
        );
        let assemble_default_var = self.fresh_var(&format!("{}_assemble_default", name));
        self.collections.insert(
            assemble_default_var.clone(),
            CollectionSpec::Literal(Value::empty_list()),
        );
        let list_var = VarId::new(name);
        self.collections.insert(
            list_var.clone(),
            CollectionSpec::HoldLatest(vec![assemble_default_var, assemble_raw_var]),
        );
        self.reactive_vars
            .insert(name.to_string(), list_var.clone());

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
            Expression::Alias(Alias::WithPassed { extra_parts }) => extra_parts
                .first()
                .map(|p| p.as_str() == name)
                .unwrap_or(false),
            Expression::Pipe { from, to } => {
                Self::expr_references_name(from, name) || Self::expr_references_name(to, name)
            }
            Expression::Then { body } => Self::expr_references_name(body, name),
            Expression::When { arms } => arms
                .iter()
                .any(|a| Self::expr_references_name(&a.body, name)),
            Expression::While { arms } => arms
                .iter()
                .any(|a| Self::expr_references_name(&a.body, name)),
            Expression::Comparator(cmp) => match cmp {
                static_expression::Comparator::Equal {
                    operand_a,
                    operand_b,
                }
                | static_expression::Comparator::NotEqual {
                    operand_a,
                    operand_b,
                }
                | static_expression::Comparator::Greater {
                    operand_a,
                    operand_b,
                }
                | static_expression::Comparator::Less {
                    operand_a,
                    operand_b,
                }
                | static_expression::Comparator::GreaterOrEqual {
                    operand_a,
                    operand_b,
                }
                | static_expression::Comparator::LessOrEqual {
                    operand_a,
                    operand_b,
                } => {
                    Self::expr_references_name(operand_a, name)
                        || Self::expr_references_name(operand_b, name)
                }
            },
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
                } => {
                    Self::expr_references_name(operand_a, name)
                        || Self::expr_references_name(operand_b, name)
                }
                ArithmeticOperator::Negate { operand } => Self::expr_references_name(operand, name),
            },
            Expression::FunctionCall { arguments, .. } => arguments.iter().any(|a| {
                a.node
                    .value
                    .as_ref()
                    .map(|v| Self::expr_references_name(v, name))
                    .unwrap_or(false)
            }),
            Expression::List { items } => items.iter().any(|v| Self::expr_references_name(v, name)),
            Expression::Object(object) => object
                .variables
                .iter()
                .any(|v| Self::expr_references_name(&v.node.value, name)),
            Expression::Block { variables, output } => {
                variables
                    .iter()
                    .any(|v| Self::expr_references_name(&v.node.value, name))
                    || Self::expr_references_name(output, name)
            }
            Expression::Latest { inputs } => {
                inputs.iter().any(|e| Self::expr_references_name(e, name))
            }
            _ => false,
        }
    }

    /// Resolve a list source expression to a VarId.
    /// The source could be a reactive variable or a static expression.
    fn resolve_list_source(&mut self, expr: &Spanned<Expression>) -> Result<VarId, String> {
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
                        if let Ok(value) = self
                            .compiler
                            .eval_static_with_scope(&var_expr, &indexmap::IndexMap::new())
                        {
                            let literal_var = self.fresh_var("static_list");
                            self.collections
                                .insert(literal_var.clone(), CollectionSpec::Literal(value));
                            return Ok(literal_var);
                        }
                    }
                }
            }
        }

        // Try to evaluate statically and create a Literal
        if let Ok(value) = self
            .compiler
            .eval_static_with_scope(expr, &indexmap::IndexMap::new())
        {
            let literal_var = self.fresh_var("static_list");
            self.collections
                .insert(literal_var.clone(), CollectionSpec::Literal(value));
            Ok(literal_var)
        } else {
            Err(format!(
                "Could not resolve list source: {:?}",
                std::mem::discriminant(&expr.node)
            ))
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
            let name = parts
                .iter()
                .map(|p| p.as_str())
                .collect::<Vec<_>>()
                .join(".");
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

    fn resolve_keyed_source_path(&self, expr: &Spanned<Expression>) -> Option<String> {
        match &expr.node {
            Expression::Alias(Alias::WithoutPassed { parts, .. }) => {
                let path = parts
                    .iter()
                    .map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                if parts.len() == 1 {
                    if let Some(prefix) = &self.scope_prefix {
                        let first_seg_is_known = self
                            .compiler
                            .variables
                            .iter()
                            .any(|(n, _)| n == &path || n.starts_with(&format!("{path}.")));
                        if !first_seg_is_known {
                            return Some(format!("{prefix}.{path}"));
                        }
                    }
                }
                Some(path)
            }
            Expression::Alias(Alias::WithPassed { extra_parts }) => Some(
                extra_parts
                    .iter()
                    .map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join("."),
            ),
            Expression::Pipe { from, .. } => self.resolve_keyed_source_path(from),
            _ => None,
        }
    }

    fn resolve_static_source_name(expr: &Spanned<Expression>) -> Option<String> {
        match &expr.node {
            Expression::Alias(Alias::WithoutPassed { parts, .. }) => Some(
                parts
                    .iter()
                    .map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join("."),
            ),
            _ => None,
        }
    }

    fn resolve_static_item_event_path(
        expr: &Spanned<Expression>,
        item_param: &str,
        item: &Value,
    ) -> Option<(InputKind, String)> {
        let Expression::Alias(Alias::WithoutPassed { parts, .. }) = &expr.node else {
            return None;
        };
        if parts.first()?.as_str() != item_param || parts.len() < 2 {
            return None;
        }

        let suffix_parts: Vec<&str> = parts[1..].iter().map(|p| p.as_str()).collect();
        let mut current = item.clone();

        for (idx, part) in suffix_parts.iter().enumerate() {
            if let Some(base) = current.get_field(LINK_PATH_FIELD).and_then(|v| v.as_text()) {
                let rest = suffix_parts[idx..].join(".");
                let full_path = if rest.is_empty() {
                    base.to_string()
                } else {
                    format!("{base}.{rest}")
                };
                return Some(Self::detect_event_kind_and_path(&full_path));
            }
            current = current.get_field(part).cloned()?;
        }

        current
            .get_field(LINK_PATH_FIELD)
            .and_then(|v| v.as_text())
            .map(|path| Self::detect_event_kind_and_path(path))
    }

    fn extract_item_event_suffix(expr: &Spanned<Expression>, item_param: &str) -> Option<String> {
        match &expr.node {
            Expression::Alias(Alias::WithoutPassed { parts, .. }) => {
                if parts.first()?.as_str() != item_param || parts.len() < 2 {
                    return None;
                }
                Some(
                    parts[1..]
                        .iter()
                        .map(|p| p.as_str())
                        .collect::<Vec<_>>()
                        .join("."),
                )
            }
            _ => None,
        }
    }

    /// Find reactive variable names referenced in an expression.
    fn find_reactive_deps_in_expr(&self, expr: &Spanned<Expression>) -> Vec<String> {
        let mut deps = Vec::new();
        let mut visited_functions = std::collections::HashSet::new();
        self.collect_reactive_deps(expr, &mut deps, &mut visited_functions);
        deps
    }

    fn collect_reactive_deps(
        &self,
        expr: &Spanned<Expression>,
        deps: &mut Vec<String>,
        visited_functions: &mut std::collections::HashSet<String>,
    ) {
        match &expr.node {
            Expression::Variable(var) => {
                self.collect_reactive_deps(&var.value, deps, visited_functions);
            }
            Expression::Alias(alias) => {
                let is_known_reactive = |name: &str| {
                    self.reactive_vars.contains_key(name)
                        || self
                            .compiler
                            .get_var_expr(name)
                            .is_some_and(|expr| self.compiler.is_reactive(expr))
                };
                // Build the full alias path and check if it (or a prefix) is a reactive var
                let path = match alias {
                    Alias::WithoutPassed { parts, .. } => parts
                        .iter()
                        .map(|p| p.as_str())
                        .collect::<Vec<_>>()
                        .join("."),
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
                if is_known_reactive(&full_path) {
                    deps.push(full_path);
                } else if is_known_reactive(&path) {
                    deps.push(path);
                } else if let Some(without_passed) = path.strip_prefix("PASSED.") {
                    // PASSED.store.X → store.X in reactive_vars
                    if is_known_reactive(without_passed) {
                        deps.push(without_passed.to_string());
                    }
                }
            }
            Expression::Pipe { from, to } => {
                self.collect_reactive_deps(from, deps, visited_functions);
                self.collect_reactive_deps(to, deps, visited_functions);
            }
            Expression::When { arms } => {
                for arm in arms {
                    self.collect_reactive_deps(&arm.body, deps, visited_functions);
                }
            }
            Expression::FunctionCall { path, arguments } => {
                for arg in arguments {
                    if let Some(ref value) = arg.node.value {
                        self.collect_reactive_deps(value, deps, visited_functions);
                    }
                }

                let fn_name = path
                    .iter()
                    .map(|segment| segment.as_str())
                    .collect::<Vec<_>>()
                    .join("/");
                if let Some((qualified_name, _params, body)) = self.compiler.find_function(&fn_name)
                {
                    if visited_functions.insert(qualified_name.clone()) {
                        self.collect_reactive_deps(body, deps, visited_functions);
                        visited_functions.remove(qualified_name.as_str());
                    }
                }
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
                } => {
                    self.collect_reactive_deps(operand_a, deps, visited_functions);
                    self.collect_reactive_deps(operand_b, deps, visited_functions);
                }
                ArithmeticOperator::Negate { operand } => {
                    self.collect_reactive_deps(operand, deps, visited_functions);
                }
            },
            Expression::Comparator(op) => match op {
                Comparator::Equal {
                    operand_a,
                    operand_b,
                }
                | Comparator::NotEqual {
                    operand_a,
                    operand_b,
                } => {
                    self.collect_reactive_deps(operand_a, deps, visited_functions);
                    self.collect_reactive_deps(operand_b, deps, visited_functions);
                }
                Comparator::Greater {
                    operand_a,
                    operand_b,
                }
                | Comparator::GreaterOrEqual {
                    operand_a,
                    operand_b,
                }
                | Comparator::Less {
                    operand_a,
                    operand_b,
                }
                | Comparator::LessOrEqual {
                    operand_a,
                    operand_b,
                } => {
                    self.collect_reactive_deps(operand_a, deps, visited_functions);
                    self.collect_reactive_deps(operand_b, deps, visited_functions);
                }
            },
            Expression::While { arms } => {
                for arm in arms {
                    self.collect_reactive_deps(&arm.body, deps, visited_functions);
                }
            }
            Expression::Then { body } => {
                self.collect_reactive_deps(body, deps, visited_functions);
            }
            Expression::Latest { inputs } => {
                for input in inputs {
                    self.collect_reactive_deps(input, deps, visited_functions);
                }
            }
            Expression::TextLiteral { parts, .. } => {
                for part in parts {
                    if let TextPart::Interpolation { var, .. } = part {
                        let path = var.as_str().to_string();
                        let full_path = if let Some(ref prefix) = self.scope_prefix {
                            format!("{}.{}", prefix, path)
                        } else {
                            path.clone()
                        };
                        let is_known_reactive = |name: &str| {
                            self.reactive_vars.contains_key(name)
                                || self
                                    .compiler
                                    .get_var_expr(name)
                                    .is_some_and(|expr| self.compiler.is_reactive(expr))
                        };
                        if is_known_reactive(&full_path) {
                            deps.push(full_path);
                        } else if is_known_reactive(&path) {
                            deps.push(path);
                        } else if let Some(without_passed) = path.strip_prefix("PASSED.") {
                            if is_known_reactive(without_passed) {
                                deps.push(without_passed.to_string());
                            }
                        }
                    }
                }
            }
            Expression::Block { variables, output } => {
                for variable in variables {
                    self.collect_reactive_deps(&variable.node.value, deps, visited_functions);
                }
                self.collect_reactive_deps(output, deps, visited_functions);
            }
            Expression::Object(obj) => {
                for variable in &obj.variables {
                    self.collect_reactive_deps(&variable.node.value, deps, visited_functions);
                }
            }
            Expression::List { items } => {
                for item in items {
                    self.collect_reactive_deps(item, deps, visited_functions);
                }
            }
            Expression::Hold { body, .. } => {
                self.collect_reactive_deps(body, deps, visited_functions);
            }
            Expression::PostfixFieldAccess { expr, .. } => {
                self.collect_reactive_deps(expr, deps, visited_functions);
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
                self.inputs
                    .iter()
                    .any(|spec| spec.id == *input_id && spec.kind == InputKind::Router)
            }
            Some(CollectionSpec::Map { source, .. }) => self.has_initial_value(source),
            Some(CollectionSpec::Then { source, .. }) => self.has_initial_value(source),
            Some(CollectionSpec::SampleOnEvent { .. }) => false,
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
            Some(CollectionSpec::CombineLatest(sources)) => {
                sources.iter().all(|s| self.has_initial_value(s))
            }
            Some(CollectionSpec::Skip { source, .. }) => self.has_initial_value(source),
            Some(CollectionSpec::SideEffect { source, .. }) => self.has_initial_value(source),
            Some(CollectionSpec::ListAssemble(source)) => self.has_initial_value(source),
            Some(CollectionSpec::ListCount(source)) => self.has_initial_value(source),
            Some(CollectionSpec::ListRetain { source, .. }) => self.has_initial_value(source),
            Some(CollectionSpec::ListRetainReactive {
                list, filter_state, ..
            }) => self.has_initial_value(list) && self.has_initial_value(filter_state),
            Some(CollectionSpec::ListMap { source, .. }) => self.has_initial_value(source),
            Some(CollectionSpec::ListMapWithKey { source, .. }) => self.has_initial_value(source),
            Some(CollectionSpec::ListMapWithKeyReactive { source, dep, .. }) => {
                self.has_initial_value(source) && self.has_initial_value(dep)
            }
            Some(CollectionSpec::ListAppend { list, .. }) => self.has_initial_value(list),
            Some(CollectionSpec::ListRemove { list, .. }) => self.has_initial_value(list),
            Some(CollectionSpec::ListLatest(source)) => {
                // ListLatest only emits when the source list has items.
                // For literal non-empty lists, it has an initial value.
                // For dynamic lists (KeyedHoldState, etc.) starting empty, it won't emit.
                if let Some(CollectionSpec::LiteralList(items)) = self.collections.get(source) {
                    !items.is_empty()
                } else {
                    false
                }
            }
            Some(CollectionSpec::ListEvery { source, .. }) => self.has_initial_value(source),
            Some(CollectionSpec::ListAny { source, .. }) => self.has_initial_value(source),
            Some(CollectionSpec::MapToKeyed { .. }) => false, // Event stream, no initial value
            Some(CollectionSpec::KeyedEventMap { .. }) => false, // Event stream, no initial value
            Some(CollectionSpec::AppendNewKeyed { .. }) => false, // Event stream, no initial value
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
        let clear_source_expr = clear_args
            .iter()
            .find(|a| a.node.name.as_str() == "on")
            .and_then(|a| a.node.value.as_ref())
            .ok_or_else(|| "List/clear missing 'on' argument".to_string())?;
        let (clear_input_var, _) = self.compile_event_source(clear_source_expr)?;

        // Unwrap the from to find List/append
        let append_source = match &from.node {
            Expression::Pipe {
                from: inner_from,
                to: inner_to,
            } => {
                match &inner_to.node {
                    Expression::FunctionCall { path, arguments }
                        if {
                            let p: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                            p.as_slice() == ["List", "append"]
                        } =>
                    {
                        // Extract the `item:` argument (the reactive var to append)
                        let item_arg = arguments
                            .iter()
                            .find(|a| {
                                a.node.name.as_str() == "item" || a.node.name.as_str() == "on"
                            })
                            .and_then(|a| a.node.value.as_ref())
                            .ok_or_else(|| {
                                "List/append missing 'item' or 'on' argument".to_string()
                            })?;

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
                body: Arc::new(|_v: &Value| Value::object([("__t", Value::text("clear"))])),
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
        self.collections.insert(
            initial_var.clone(),
            CollectionSpec::Literal(initial_list.clone()),
        );

        // Check for persisted items
        let effective_initial = self
            .persisted_holds
            .get(name)
            .cloned()
            .unwrap_or_else(|| initial_list.clone());
        // Override initial collection if persisted
        if self.persisted_holds.contains_key(name) {
            self.collections.insert(
                initial_var.clone(),
                CollectionSpec::Literal(effective_initial.clone()),
            );
        }

        let list_var = VarId::new(name);
        self.collections.insert(
            list_var.clone(),
            CollectionSpec::HoldState {
                initial: initial_var,
                events: events_var,
                initial_value: effective_initial,
                transform: Arc::new(|state: &Value, event: &Value| {
                    let event_type = event
                        .get_field("__t")
                        .and_then(|v| v.as_text())
                        .unwrap_or("")
                        .to_string();
                    match event_type.as_str() {
                        "append" => {
                            let item = event.get_field("v").cloned().unwrap_or(Value::Unit);
                            let count = state.list_count();
                            state.list_append(item, count)
                        }
                        "remove_last" => state.list_remove_last(),
                        "clear" => Value::empty_list(),
                        _ => state.clone(),
                    }
                }),
            },
        );

        self.reactive_vars
            .insert(name.to_string(), list_var.clone());

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
        let item_arg = arguments
            .iter()
            .find(|a| a.node.name.as_str() == "item" || a.node.name.as_str() == "on")
            .and_then(|a| a.node.value.as_ref())
            .ok_or_else(|| "List/append missing 'item' or 'on' argument".to_string())?;
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
        self.collections.insert(
            initial_var.clone(),
            CollectionSpec::Literal(initial_list.clone()),
        );

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

        self.reactive_vars
            .insert(name.to_string(), list_var.clone());
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
        let existing = obj
            .get_field(parts[0])
            .cloned()
            .unwrap_or_else(|| Value::Object(Arc::new(BTreeMap::new())));
        let updated = set_nested_field(&existing, parts[1], value);
        obj.update_field(parts[0], updated)
    }
}

fn inject_scope_path(scope: &mut IndexMap<String, Value>, path: &str, value: Value) {
    let root = set_nested_field(&Value::Object(Arc::new(BTreeMap::new())), path, value);
    if let Value::Object(fields) = root {
        for (key, val) in fields.iter() {
            scope.insert(key.as_ref().to_string(), val.clone());
        }
    }
}

fn inject_scope_path_merged(scope: &mut IndexMap<String, Value>, path: &str, value: Value) {
    let mut parts = path.splitn(2, '.');
    let Some(root_name) = parts.next() else {
        return;
    };
    let updated = if let Some(rest) = parts.next() {
        let existing = scope
            .get(root_name)
            .cloned()
            .unwrap_or_else(|| Value::Object(Arc::new(BTreeMap::new())));
        set_nested_field(&existing, rest, value)
    } else {
        value
    };
    scope.insert(root_name.to_string(), updated);
}

fn decorate_reactive_scope_value(value: &Value, base_path: &str) -> Value {
    match value {
        Value::Tagged { tag, fields } if tag.as_ref() == LIST_TAG => {
            let decorated_fields: BTreeMap<Arc<str>, Value> = fields
                .iter()
                .map(|(key, item)| {
                    let item_path = format!("{base_path}.{}", key.as_ref());
                    let decorated = decorate_reactive_scope_value(item, &item_path);
                    (key.clone(), decorated)
                })
                .collect();
            Value::Tagged {
                tag: tag.clone(),
                fields: Arc::new(decorated_fields),
            }
        }
        Value::Object(fields) => {
            let mut result = value.clone();

            if value.get_field(HOVERED_FIELD).is_none() {
                result = result.update_field(HOVERED_FIELD, Value::tag("False"));
            }
            result = result.update_field(
                HOVER_PATH_FIELD,
                Value::text(format!("{base_path}.hovered").as_str()),
            );

            for (field_name, field_value) in fields.iter() {
                if field_name.ends_with("_elements") {
                    let mut new_elements = field_value.clone();
                    if let Value::Object(element_fields) = field_value {
                        for (el_name, existing) in element_fields.iter() {
                            let link_path = format!("{base_path}.{}.{}", field_name, el_name);
                            let updated = match existing {
                                Value::Object(_) => existing
                                    .update_field(LINK_PATH_FIELD, Value::text(link_path.as_str())),
                                _ => Value::object([(
                                    LINK_PATH_FIELD,
                                    Value::text(link_path.as_str()),
                                )]),
                            };
                            new_elements = new_elements.update_field(el_name.as_ref(), updated);
                        }
                    }
                    result = result.update_field(field_name.as_ref(), new_elements);
                    continue;
                }

                let child_path = format!("{base_path}.{}", field_name);
                let decorated = decorate_reactive_scope_value(field_value, &child_path);
                result = result.update_field(field_name.as_ref(), decorated);
            }

            result
        }
        Value::Tagged { tag, fields } => {
            let decorated_fields: BTreeMap<Arc<str>, Value> = fields
                .iter()
                .map(|(field_name, field_value)| {
                    let child_path = format!("{base_path}.{}", field_name);
                    (
                        field_name.clone(),
                        decorate_reactive_scope_value(field_value, &child_path),
                    )
                })
                .collect();
            Value::Tagged {
                tag: tag.clone(),
                fields: Arc::new(decorated_fields),
            }
        }
        _ => value.clone(),
    }
}

fn value_needs_reactive_decoration(value: &Value) -> bool {
    match value {
        Value::Tag(tag) => tag.as_ref() == "LINK",
        Value::Object(fields) => fields.iter().any(|(field_name, field_value)| {
            field_name.ends_with("_elements") || value_needs_reactive_decoration(field_value)
        }),
        Value::Tagged { fields, .. } => fields.values().any(value_needs_reactive_decoration),
        _ => false,
    }
}

/// Inject __link_path__ fields into a list item with a specific key.
///
/// Any object field ending with `_elements` is treated as a per-item element
/// namespace and receives item-specific `__link_path__` values for each LINK.
fn inject_item_link_paths_with_key(item: &Value, list_path: &str, key: &str) -> Value {
    let item_path = format!("{}.{}", list_path, key);
    let mut result = decorate_reactive_scope_value(item, &item_path);

    // While editing, render the in-progress edit buffer via `title`.
    // The TodoMVC edit input uses `LATEST { todo.title, element.event.change.text }`.
    // In keyed DD rendering, this expression is re-evaluated from item state, so
    // `element.event.change.text` is not persistent between updates. Mirroring
    // `__edit_text` into `title` during edit mode preserves incremental typing.
    let editing = result
        .get_field("editing")
        .and_then(|v| v.as_tag())
        .map(|tag| tag == "True")
        .unwrap_or(false);
    if editing {
        if let Some(edit_text) = result.get_field("__edit_text").and_then(|v| v.as_text()) {
            result = result.update_field("title", Value::text(edit_text));
        }
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
    fn has_reactive_ref(&self) -> bool {
        match self {
            DocTemplate::ReactiveRef => true,
            DocTemplate::Static(_) => false,
            DocTemplate::Tagged { fields, .. } => fields.iter().any(|(_, t)| t.has_reactive_ref()),
            DocTemplate::TextInterpolation(parts) => parts
                .iter()
                .any(|p| matches!(p, TextPartTemplate::ReactiveRef)),
        }
    }

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
                    ["Scene", "new"] => {
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
                            tag: "SceneNew".to_string(),
                            fields: field_templates,
                        })
                    }
                    ["Light", "directional"] => {
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
                            tag: "DirectionalLight".to_string(),
                            fields: field_templates,
                        })
                    }
                    ["Light", "ambient"] => {
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
                            tag: "AmbientLight".to_string(),
                            fields: field_templates,
                        })
                    }
                    ["Light", "spot"] => {
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
                            tag: "SpotLight".to_string(),
                            fields: field_templates,
                        })
                    }
                    ["Element", elem_type] | ["Scene", "Element", elem_type] => {
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
                                            // Mark this Stripe as keyed so the bridge can identify it
                                            field_templates.push((
                                                "__keyed__".to_string(),
                                                DocTemplate::Static(Value::tag("True")),
                                            ));
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

            Expression::Pipe { from, to } => match &to.node {
                Expression::FunctionCall { .. } => self
                    .build_doc_template_inner(reactive_var_name, to, keyed_list_name)
                    .or_else(|_| {
                        let val = self.eval_static(expr).unwrap_or(Value::Unit);
                        Ok(DocTemplate::Static(val))
                    }),
                _ => {
                    let val = self.eval_static(expr).unwrap_or(Value::Unit);
                    Ok(DocTemplate::Static(val))
                }
            },

            Expression::List { items } => {
                let mut item_templates = Vec::new();
                for (i, item) in items.iter().enumerate() {
                    let tmpl =
                        self.build_doc_template_inner(reactive_var_name, item, keyed_list_name)?;
                    item_templates.push((format!("{:04}", i), tmpl));
                }
                Ok(DocTemplate::Tagged {
                    tag: LIST_TAG.to_string(),
                    fields: item_templates,
                })
            }

            Expression::TextLiteral { parts, .. } => {
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

            Expression::Link => Ok(DocTemplate::Static(Value::tag("LINK"))),

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

            Expression::Literal(_) | Expression::TaggedObject { .. } => {
                let val = self.eval_static(expr)?;
                Ok(DocTemplate::Static(val))
            }

            _ => match self.eval_static(expr) {
                Ok(val) => Ok(DocTemplate::Static(val)),
                Err(e) => Err(format!("Cannot build doc template: {}", e)),
            },
        }
    }

    fn find_link_path_for_element(
        &self,
        _reactive_var_name: &str,
        _element_expr: &Spanned<Expression>,
    ) -> Option<String> {
        // Walk through top-level variables to find which one contains this LINK
        for (name, expr) in self.variables.iter() {
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
        for (_, _, body) in self.functions.iter() {
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
                let is_stripe = matches!(
                    path_strs.as_slice(),
                    ["Element", "stripe"] | ["Scene", "Element", "stripe"]
                );
                if is_stripe {
                    // Check if items: references the keyed list
                    let has_keyed_items = arguments.iter().any(|arg| {
                        arg.node.name.as_str() == "items"
                            && arg.node.value.as_ref().map_or(false, |v| {
                                self.expr_references_passed_store(v, keyed_list_short_name)
                            })
                    });
                    if has_keyed_items {
                        // Extract tag from element: [tag: X], or use empty string if no tag
                        for arg in arguments {
                            if arg.node.name.as_str() == "element" {
                                if let Some(ref val_expr) = arg.node.value {
                                    if let Ok(val) = self.eval_static(val_expr) {
                                        if let Some(tag) =
                                            val.get_field("tag").and_then(|t| t.as_tag())
                                        {
                                            return Some(tag.to_string());
                                        }
                                    }
                                }
                                // element: [] or element: [no tag] — use empty sentinel
                                return Some(String::new());
                            }
                        }
                        // No element arg at all — use empty sentinel
                        return Some(String::new());
                    }
                }
                // Recurse into arguments
                for arg in arguments {
                    if let Some(ref val_expr) = arg.node.value {
                        if let Some(tag) =
                            self.find_keyed_stripe_tag_in_expr(val_expr, keyed_list_short_name)
                        {
                            return Some(tag);
                        }
                    }
                }
                None
            }
            Expression::Pipe { from, to } => self
                .find_keyed_stripe_tag_in_expr(from, keyed_list_short_name)
                .or_else(|| self.find_keyed_stripe_tag_in_expr(to, keyed_list_short_name)),
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
            Expression::FunctionCall { arguments, .. } => arguments.iter().any(|a| {
                a.node
                    .value
                    .as_ref()
                    .map(|v| self.expr_contains_link(v))
                    .unwrap_or(false)
            }),
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

#[cfg(test)]
mod tests {
    use super::{
        CollectionSpec, CompiledProgram, Compiler, LIST_TAG, compile,
        decorate_reactive_scope_value, inject_item_link_paths_with_key, parse_source,
    };
    use crate::parser::static_expression::Expression;
    use crate::platform::browser::engine_dd::core::types::{
        DEP_FIELD_PREFIX, InputKind, LINK_PATH_FIELD, ListKey, VarId,
    };
    use crate::platform::browser::engine_dd::core::value::Value;
    use boon_scene::RenderSurface;
    use indexmap::IndexMap;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn read_example(path: &str) -> String {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(base.join(path)).expect("read example source")
    }

    fn list_value(items: Vec<Value>) -> Value {
        let fields: BTreeMap<Arc<str>, Value> = items
            .into_iter()
            .enumerate()
            .map(|(i, value)| (Arc::from(format!("{:04}", i)), value))
            .collect();
        Value::Tagged {
            tag: Arc::from(super::LIST_TAG),
            fields: Arc::new(fields),
        }
    }

    #[test]
    fn cells_formula_helpers_resolve_mocked_cell_values() {
        let mut source = read_example("../../playground/frontend/src/examples/cells/cells.bn");
        source.push_str(
            r#"

probe_formula_a1: cell_formula(column: 1, row: 1)
probe_formula_a2: cell_formula(column: 1, row: 2)
probe_formula_a3: cell_formula(column: 1, row: 3)
probe_value_a1: compute_value(formula_text: cell_formula(column: 1, row: 1))
probe_value_a2: compute_value(formula_text: cell_formula(column: 1, row: 2))
probe_value_a3: compute_value(formula_text: cell_formula(column: 1, row: 3))
probe_add_expression: expression_value(text: TEXT { add(A1, A2) })
probe_add_left_ref: BLOCK {
    text: TEXT { add(A1, A2) }
    comma_index: text |> Text/find(search: TEXT { , })
    left_length: comma_index - 4
    text |> Text/substring(start: 4, length: left_length) |> Text/trim()
}
probe_add_right_ref: BLOCK {
    text: TEXT { add(A1, A2) }
    comma_index: text |> Text/find(search: TEXT { , })
    right_length: text |> Text/length() - comma_index - 2
    text |> Text/substring(start: comma_index + 1, length: right_length) |> Text/trim()
}
probe_add_left_value: BLOCK {
    left_ref: TEXT { A1 }
    left_column: left_ref |> Text/substring(start: 0, length: 1) |> parse_column_letter()
    left_row: left_ref |> Text/substring(start: 1, length: 8) |> Text/to_number()
    compute_value(formula_text: cell_formula(column: left_column, row: left_row))
}
probe_add_right_value: BLOCK {
    right_ref: TEXT { A2 }
    right_column: right_ref |> Text/substring(start: 0, length: 1) |> parse_column_letter()
    right_row: right_ref |> Text/substring(start: 1, length: 8) |> Text/to_number()
    compute_value(formula_text: cell_formula(column: right_column, row: right_row))
}
probe_sum_expression: expression_value(text: TEXT { sum(A1:A3) })
probe_sum_start_ref: BLOCK {
    text: TEXT { sum(A1:A3) }
    text_length: text |> Text/length()
    colon_index: text |> Text/find(search: TEXT { : })
    start_ref_length: colon_index - 4
    text |> Text/substring(start: 4, length: start_ref_length) |> Text/trim()
}
probe_sum_end_ref: BLOCK {
    text: TEXT { sum(A1:A3) }
    text_length: text |> Text/length()
    colon_index: text |> Text/find(search: TEXT { : })
    end_ref_length: text_length - colon_index - 2
    text |> Text/substring(start: colon_index + 1, length: end_ref_length) |> Text/trim()
}
probe_sum_range_value: BLOCK {
    sum_range(column: 1, start_row: 1, end_row: 3)
}
probe_add: compute_value(formula_text: TEXT { =add(A1, A2) })
probe_sum: compute_value(formula_text: TEXT { =sum(A1:A3) })
probe_display_cell: make_cell_element(cell: [column: 1 row: 1 cell_elements: [display: LINK editing: LINK]])
probe_display_row_elements: make_row_elements(row_cells: row_1_cells)
probe_display_row: make_row(row_index: 1, row_cells: row_1_cells)
"#,
        );

        let ast = parse_source(&source).expect("cells source should parse");
        let mut compiler = Compiler::new();
        compiler.register_top_level(&ast);

        let mocked_cells = list_value(vec![
            list_value(vec![
                Value::text("5"),
                Value::text("=add(A1, A2)"),
                Value::text("=sum(A1:A3)"),
            ]),
            list_value(vec![Value::text("10"), Value::text(""), Value::text("")]),
            list_value(vec![Value::text("15"), Value::text(""), Value::text("")]),
        ]);

        let mut scope = IndexMap::new();
        scope.insert("sheet".to_string(), mocked_cells);

        let probe_formula_a1 = compiler
            .get_var_expr("probe_formula_a1")
            .expect("probe_formula_a1 expr");
        let probe_formula_a2 = compiler
            .get_var_expr("probe_formula_a2")
            .expect("probe_formula_a2 expr");
        let probe_formula_a3 = compiler
            .get_var_expr("probe_formula_a3")
            .expect("probe_formula_a3 expr");
        let probe_value_a1 = compiler
            .get_var_expr("probe_value_a1")
            .expect("probe_value_a1 expr");
        let probe_value_a2 = compiler
            .get_var_expr("probe_value_a2")
            .expect("probe_value_a2 expr");
        let probe_value_a3 = compiler
            .get_var_expr("probe_value_a3")
            .expect("probe_value_a3 expr");
        let probe_add = compiler.get_var_expr("probe_add").expect("probe_add expr");
        let probe_add_expression = compiler
            .get_var_expr("probe_add_expression")
            .expect("probe_add_expression expr");
        let probe_add_left_ref = compiler
            .get_var_expr("probe_add_left_ref")
            .expect("probe_add_left_ref expr");
        let probe_add_right_ref = compiler
            .get_var_expr("probe_add_right_ref")
            .expect("probe_add_right_ref expr");
        let probe_add_left_value = compiler
            .get_var_expr("probe_add_left_value")
            .expect("probe_add_left_value expr");
        let probe_add_right_value = compiler
            .get_var_expr("probe_add_right_value")
            .expect("probe_add_right_value expr");
        let probe_sum_expression = compiler
            .get_var_expr("probe_sum_expression")
            .expect("probe_sum_expression expr");
        let probe_sum_start_ref = compiler
            .get_var_expr("probe_sum_start_ref")
            .expect("probe_sum_start_ref expr");
        let probe_sum_end_ref = compiler
            .get_var_expr("probe_sum_end_ref")
            .expect("probe_sum_end_ref expr");
        let probe_sum_range_value = compiler
            .get_var_expr("probe_sum_range_value")
            .expect("probe_sum_range_value expr");
        let probe_sum = compiler.get_var_expr("probe_sum").expect("probe_sum expr");
        let probe_display_cell = compiler
            .get_var_expr("probe_display_cell")
            .expect("probe_display_cell expr");
        let probe_display_row_elements = compiler
            .get_var_expr("probe_display_row_elements")
            .expect("probe_display_row_elements expr");
        let probe_display_row = compiler
            .get_var_expr("probe_display_row")
            .expect("probe_display_row expr");
        let row_1_formulas = compiler
            .get_var_expr("row_1_formulas")
            .expect("row_1_formulas expr");

        assert_eq!(
            compiler
                .eval_static_with_scope(probe_formula_a1, &scope)
                .ok()
                .and_then(|v| v.as_text().map(str::to_string)),
            Some("5".to_string()),
            "cell_formula(1,1) should resolve A1 formula text from mocked sheet"
        );
        let row_1_formula_items = compiler
            .eval_static_with_scope(row_1_formulas, &scope)
            .expect("row_1_formulas should evaluate")
            .list_items()
            .into_iter()
            .take(3)
            .map(|value| value.as_text().unwrap_or("?").to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            row_1_formula_items,
            vec![
                "5".to_string(),
                "=add(A1, A2)".to_string(),
                "=sum(A1:A3)".to_string()
            ],
            "row_1_formulas should seed A1/B1/C1 formulas"
        );
        assert_eq!(
            compiler
                .eval_static_with_scope(probe_formula_a2, &scope)
                .ok()
                .and_then(|v| v.as_text().map(str::to_string)),
            Some("10".to_string()),
            "cell_formula(1,2) should resolve A2 formula text from mocked sheet"
        );
        assert_eq!(
            compiler
                .eval_static_with_scope(probe_formula_a3, &scope)
                .ok()
                .and_then(|v| v.as_text().map(str::to_string)),
            Some("15".to_string()),
            "cell_formula(1,3) should resolve A3 formula text from mocked sheet"
        );
        assert_eq!(
            compiler
                .eval_static_with_scope(probe_value_a1, &scope)
                .ok()
                .and_then(|v| v.as_number()),
            Some(5.0),
            "compute_value(cell_formula(1,1)) should resolve A1 to 5"
        );
        assert_eq!(
            compiler
                .eval_static_with_scope(probe_value_a2, &scope)
                .ok()
                .and_then(|v| v.as_number()),
            Some(10.0),
            "compute_value(cell_formula(1,2)) should resolve A2 to 10"
        );
        assert_eq!(
            compiler
                .eval_static_with_scope(probe_value_a3, &scope)
                .ok()
                .and_then(|v| v.as_number()),
            Some(15.0),
            "compute_value(cell_formula(1,3)) should resolve A3 to 15"
        );
        eprintln!(
            "[cells-dd] probe_add_expression={:?} left_ref={:?} right_ref={:?} left_value={:?} right_value={:?} add={:?} sum_expression={:?} sum_start_ref={:?} sum_end_ref={:?} sum_range={:?} sum={:?}",
            compiler
                .eval_static_with_scope(probe_add_expression, &scope)
                .ok(),
            compiler
                .eval_static_with_scope(probe_add_left_ref, &scope)
                .ok(),
            compiler
                .eval_static_with_scope(probe_add_right_ref, &scope)
                .ok(),
            compiler
                .eval_static_with_scope(probe_add_left_value, &scope)
                .ok(),
            compiler
                .eval_static_with_scope(probe_add_right_value, &scope)
                .ok(),
            compiler.eval_static_with_scope(probe_add, &scope).ok(),
            compiler
                .eval_static_with_scope(probe_sum_expression, &scope)
                .ok(),
            compiler
                .eval_static_with_scope(probe_sum_start_ref, &scope)
                .ok(),
            compiler
                .eval_static_with_scope(probe_sum_end_ref, &scope)
                .ok(),
            compiler
                .eval_static_with_scope(probe_sum_range_value, &scope)
                .ok(),
            compiler.eval_static_with_scope(probe_sum, &scope).ok(),
        );
        assert_eq!(
            compiler
                .eval_static_with_scope(probe_add, &scope)
                .ok()
                .and_then(|v| v.as_number()),
            Some(15.0),
            "compute_value(add(A1, A2)) should evaluate to 15"
        );
        assert_eq!(
            compiler
                .eval_static_with_scope(probe_sum, &scope)
                .ok()
                .and_then(|v| v.as_number()),
            Some(30.0),
            "compute_value(sum(A1:A3)) should evaluate to 30"
        );

        scope.insert(
            "editing_cell".to_string(),
            Value::object([("row", Value::number(0.0)), ("column", Value::number(0.0))]),
        );
        scope.insert("editing_text".to_string(), Value::text(""));
        let probe_display_cell_value = compiler
            .eval_static_with_scope(probe_display_cell, &scope)
            .expect("display cell should evaluate");
        assert_eq!(
            probe_display_cell_value.get_tag(),
            Some("ElementLabel"),
            "make_cell_element should render a label when not editing; got {probe_display_cell_value}"
        );
        assert_eq!(
            probe_display_cell_value
                .get_field("label")
                .and_then(Value::as_text),
            Some("5"),
            "A1 display cell should render the computed value 5; got {probe_display_cell_value}"
        );

        let probe_display_row_elements_value = compiler
            .eval_static_with_scope(probe_display_row_elements, &scope)
            .expect("display row elements should evaluate");
        let probe_display_row_items = probe_display_row_elements_value.list_items();
        assert_eq!(
            probe_display_row_items
                .get(0)
                .and_then(|v| v.get_field("label"))
                .and_then(Value::as_text),
            Some("5"),
            "row_1 first cell should render 5; got {probe_display_row_elements_value}"
        );
        assert_eq!(
            probe_display_row_items
                .get(1)
                .and_then(|v| v.get_field("label"))
                .and_then(Value::as_text),
            Some("15"),
            "row_1 second cell should render 15; got {probe_display_row_elements_value}"
        );
        assert_eq!(
            probe_display_row_items
                .get(2)
                .and_then(|v| v.get_field("label"))
                .and_then(Value::as_text),
            Some("30"),
            "row_1 third cell should render 30; got {probe_display_row_elements_value}"
        );

        let probe_display_row_value = compiler
            .eval_static_with_scope(probe_display_row, &scope)
            .expect("display row should evaluate");
        let nested_row_items = probe_display_row_value
            .get_field("items")
            .expect("row stripe should have items")
            .list_items();
        let nested_cells = nested_row_items
            .get(1)
            .and_then(|v| v.get_field("items"))
            .expect("row stripe should have nested cell stripe items")
            .list_items();
        assert_eq!(
            nested_cells
                .get(0)
                .and_then(|v| v.get_field("label"))
                .and_then(Value::as_text),
            Some("5"),
            "nested row first cell should render 5; got {probe_display_row_value}"
        );

        scope.insert(
            "editing_cell".to_string(),
            Value::object([("row", Value::number(1.0)), ("column", Value::number(1.0))]),
        );
        scope.insert("editing_text".to_string(), Value::text("5"));
        assert_eq!(
            compiler
                .eval_static_with_scope(probe_display_cell, &scope)
                .ok()
                .and_then(|v| v.get_tag().map(str::to_string)),
            Some("ElementTextInput".to_string()),
            "make_cell_element should render a text input while editing A1"
        );
    }

    #[test]
    fn cells_sheet_hold_transform_updates_a1_formula_text() {
        let source = read_example("../../playground/frontend/src/examples/cells/cells.bn");
        let ast = parse_source(&source).expect("cells source should parse");
        let mut compiler = Compiler::new();
        compiler.register_top_level(&ast);

        let sheet_expr = compiler.get_var_expr("sheet").expect("sheet expr");
        let then_expr = match &sheet_expr.node {
            Expression::Pipe { to, .. } => match &to.node {
                Expression::Hold { body, .. } => match &body.node {
                    Expression::Pipe { to, .. } => match &to.node {
                        Expression::Then { body } => body.as_ref().clone(),
                        other => panic!("expected THEN in sheet HOLD body, got {other:?}"),
                    },
                    other => panic!("expected pipe in sheet HOLD body, got {other:?}"),
                },
                other => panic!("expected HOLD in sheet expr, got {other:?}"),
            },
            other => panic!("expected pipe in sheet expr, got {other:?}"),
        };

        let mut rows = Vec::new();
        for row in 1..=100 {
            let mut cols = Vec::new();
            for col in 1..=26 {
                let text = match (row, col) {
                    (1, 1) => "5",
                    (1, 2) => "=add(A1, A2)",
                    (1, 3) => "=sum(A1:A3)",
                    (2, 1) => "10",
                    (3, 1) => "15",
                    _ => "",
                };
                cols.push(Value::text(text));
            }
            rows.push(list_value(cols));
        }

        let mut scope = IndexMap::new();
        scope.insert("state".to_string(), list_value(rows));
        scope.insert(
            "edit_committed".to_string(),
            Value::object([
                ("row", Value::number(1.0)),
                ("column", Value::number(1.0)),
                ("text", Value::text("7")),
            ]),
        );

        let result = compiler.eval_static_with_scope(&then_expr, &scope);
        assert!(
            result.is_ok(),
            "sheet HOLD transform THEN body should evaluate with scoped state + edit_committed, got: {result:?}"
        );

        let result = result.expect("sheet HOLD transform result");
        let rows = result.list_items();
        assert!(
            rows.len() >= 3,
            "updated sheet should still be a list of rows, got: {result}"
        );
        let row_1 = rows[0].list_items();
        let row_2 = rows[1].list_items();
        let row_3 = rows[2].list_items();
        assert_eq!(row_1[0].as_text(), Some("7"));
        assert_eq!(row_1[1].as_text(), Some("=add(A1, A2)"));
        assert_eq!(row_1[2].as_text(), Some("=sum(A1:A3)"));
        assert_eq!(row_2[0].as_text(), Some("10"));
        assert_eq!(row_3[0].as_text(), Some("15"));
    }

    #[test]
    fn cells_compile_to_dataflow_graph() {
        let source = read_example("../../playground/frontend/src/examples/cells/cells.bn");
        eprintln!("[cells-dd] before compile()");
        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("cells should compile");
        eprintln!("[cells-dd] after compile()");
        match program {
            CompiledProgram::Dataflow { graph } => {
                assert!(
                    !graph.collections.is_empty(),
                    "cells should produce non-empty DD collections"
                );
            }
            CompiledProgram::Static { .. } => {
                panic!("cells should compile to dataflow")
            }
        }
    }

    #[test]
    fn cells_document_eval_with_overrides_keeps_committed_values_after_cancel_state() {
        let source = read_example("../../playground/frontend/src/examples/cells/cells.bn");
        let ast = parse_source(&source).expect("cells source should parse");
        let mut compiler = Compiler::new();
        compiler.register_top_level(&ast);

        let document_expr = compiler
            .get_var_expr("document")
            .expect("document expr should exist");

        let mut scope = IndexMap::new();
        scope.insert(
            "editing_cell".to_string(),
            Value::object([("row", Value::number(0.0)), ("column", Value::number(0.0))]),
        );
        scope.insert("editing_text".to_string(), Value::text(""));
        scope.insert(
            "edit_changed".to_string(),
            Value::object([("text", Value::text(""))]),
        );
        scope.insert("edit_cancelled".to_string(), Value::Bool(true));
        scope.insert(
            "overrides".to_string(),
            list_value(vec![Value::object([
                ("row", Value::number(1.0)),
                ("column", Value::number(1.0)),
                ("text", Value::text("7")),
            ])]),
        );

        let document_value = compiler
            .eval_static_with_scope(document_expr, &scope)
            .expect("document should evaluate with override scope");

        let root = document_value
            .get_field("root")
            .expect("document should have root");
        let top_items = root
            .get_field("items")
            .expect("root should have items")
            .list_items();
        let body = top_items
            .get(2)
            .expect("document body should be third item");
        let rows = body
            .get_field("items")
            .expect("body should have row items")
            .list_items();
        let row_1 = rows.first().expect("row 1 should exist");
        let row_1_items = row_1
            .get_field("items")
            .expect("row 1 should have items")
            .list_items();
        let cells = row_1_items
            .get(1)
            .and_then(|v| v.get_field("items"))
            .expect("row 1 should have cell stripe items")
            .list_items();

        assert_eq!(
            cells
                .first()
                .and_then(|v| v.get_field("label"))
                .and_then(Value::as_text),
            Some("7"),
            "A1 should render committed override after cancel-state scope; got {document_value}"
        );
        assert_eq!(
            cells
                .get(1)
                .and_then(|v| v.get_field("label"))
                .and_then(Value::as_text),
            Some("17"),
            "B1 should recompute from committed override after cancel-state scope; got {document_value}"
        );
        assert_eq!(
            cells
                .get(2)
                .and_then(|v| v.get_field("label"))
                .and_then(Value::as_text),
            Some("32"),
            "C1 should recompute from committed override after cancel-state scope; got {document_value}"
        );
    }

    #[test]
    fn cells_compiled_document_map_keeps_committed_values_after_cancel_state() {
        let source = read_example("../../playground/frontend/src/examples/cells/cells.bn");
        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("cells should compile");
        let CompiledProgram::Dataflow { graph } = program else {
            panic!("cells should compile to dataflow");
        };

        let document_spec = graph
            .collections
            .get(&graph.document)
            .expect("document collection should exist");
        let CollectionSpec::Map { f, .. } = document_spec else {
            panic!("document collection should be a Map, got different spec");
        };

        let combined = Value::object([
            ("__dep_0", Value::object([("text", Value::text(""))])),
            ("__dep_1", Value::text("")),
            ("__dep_2", Value::Bool(true)),
            (
                "__dep_3",
                Value::object([("row", Value::number(0.0)), ("column", Value::number(0.0))]),
            ),
            (
                "__dep_4",
                list_value(vec![Value::object([
                    ("row", Value::number(1.0)),
                    ("column", Value::number(1.0)),
                    ("text", Value::text("7")),
                ])]),
            ),
        ]);

        let document_value = f(&combined);
        let root = document_value
            .get_field("root")
            .expect("document should have root");
        let top_items = root
            .get_field("items")
            .expect("root should have items")
            .list_items();
        let body = top_items
            .get(2)
            .expect("document body should be third item");
        let rows = body
            .get_field("items")
            .expect("body should have row items")
            .list_items();
        let row_1 = rows.first().expect("row 1 should exist");
        let row_1_items = row_1
            .get_field("items")
            .expect("row 1 should have items")
            .list_items();
        let cells = row_1_items
            .get(1)
            .and_then(|v| v.get_field("items"))
            .expect("row 1 should have cell stripe items")
            .list_items();

        assert_eq!(
            cells
                .first()
                .and_then(|v| v.get_field("label"))
                .and_then(Value::as_text),
            Some("7"),
            "compiled document closure should render committed A1 override; got {document_value}"
        );
        assert_eq!(
            cells
                .get(1)
                .and_then(|v| v.get_field("label"))
                .and_then(Value::as_text),
            Some("17"),
            "compiled document closure should recompute B1 from committed override; got {document_value}"
        );
        assert_eq!(
            cells
                .get(2)
                .and_then(|v| v.get_field("label"))
                .and_then(Value::as_text),
            Some("32"),
            "compiled document closure should recompute C1 from committed override; got {document_value}"
        );
    }

    #[test]
    fn static_scene_compile_preserves_lights_and_geometry() {
        let source = r#"
scene: Scene/new(
    root: Scene/Element/text(
        element: []
        style: []
        text: TEXT { hi }
    )
    lights: LIST {
        Light/directional(
            azimuth: 30
            altitude: 45
            spread: 1
            intensity: 1.2
            color: Oklch[lightness: 0.98, chroma: 0.015, hue: 65]
        )
        Light/ambient(
            intensity: 0.4
            color: Oklch[lightness: 0.8, chroma: 0.01, hue: 220]
        )
        Light/spot(
            target: FocusedElement
            color: Oklch[lightness: 0.7, chroma: 0.1, hue: 220]
            intensity: 0.3
            radius: 60
            softness: 0.85
        )
    }
    geometry: [
        bevel_angle: 45
    ]
)
"#;

        let program = compile(source, None, &std::collections::HashMap::new(), None)
            .expect("compile should succeed");

        let CompiledProgram::Static {
            document_value,
            render_surface,
        } = program
        else {
            panic!("expected static program");
        };

        assert_eq!(render_surface, RenderSurface::Scene);
        assert_eq!(document_value.get_tag(), Some("SceneNew"));
        assert_eq!(
            document_value
                .get_field("geometry")
                .and_then(|v| v.get_field("bevel_angle"))
                .and_then(|v| v.as_number()),
            Some(45.0)
        );

        let lights = document_value
            .get_field("lights")
            .expect("scene should preserve lights");
        let light_tags: Vec<_> = lights
            .list_items()
            .into_iter()
            .filter_map(|item| item.get_tag().map(str::to_string))
            .collect();
        assert_eq!(
            light_tags,
            vec![
                "DirectionalLight".to_string(),
                "AmbientLight".to_string(),
                "SpotLight".to_string(),
            ]
        );
    }

    #[test]
    fn reactive_scene_root_map_keeps_static_lights_and_geometry() {
        let source = r#"
scene: Scene/new(
    root: counter
    lights: LIST {
        Light/directional(
            azimuth: 30
            altitude: 45
            spread: 1
            intensity: 1.2
            color: Oklch[lightness: 0.98, chroma: 0.015, hue: 65]
        )
        Light/ambient(
            intensity: 0.4
            color: Oklch[lightness: 0.8, chroma: 0.01, hue: 220]
        )
    }
    geometry: [
        bevel_angle: 45
    ]
)

counter:
    LATEST {
        0
        increment_button.event.press |> THEN { 1 }
    }
    |> Math/sum()

increment_button: Element/button(
    element: [event: [press: LINK]]
    style: []
    label: TEXT { + }
)
"#;

        let program = compile(source, None, &std::collections::HashMap::new(), None)
            .expect("compile should succeed");

        let CompiledProgram::Dataflow { graph } = program else {
            panic!("expected reactive program");
        };

        assert_eq!(graph.render_surface, RenderSurface::Scene);

        let CollectionSpec::Map { f, .. } = graph
            .collections
            .get(&graph.document)
            .expect("document collection should exist")
        else {
            panic!("expected document map");
        };

        let scene = f(&Value::number(7.0));
        assert_eq!(scene.get_tag(), Some("SceneNew"));
        assert_eq!(
            scene.get_field("root").and_then(|value| value.as_number()),
            Some(7.0)
        );
        assert_eq!(
            scene
                .get_field("geometry")
                .and_then(|v| v.get_field("bevel_angle"))
                .and_then(|v| v.as_number()),
            Some(45.0)
        );
        let lights = scene.get_field("lights").expect("lights field");
        assert_eq!(lights.list_count(), 2);
        assert_eq!(
            lights.list_items().first().and_then(|item| item.get_tag()),
            Some("DirectionalLight")
        );
    }

    #[test]
    fn crud_example_compiles_as_dataflow() {
        let source = read_example("../../playground/frontend/src/examples/crud/crud.bn");

        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("crud should compile");

        let CompiledProgram::Dataflow { graph } = program else {
            panic!("expected CRUD to compile as dataflow");
        };

        assert_eq!(graph.render_surface, RenderSurface::Document);
        assert!(
            graph
                .collections
                .keys()
                .any(|var| var.as_str() == "store.people"),
            "store.people should be compiled into the graph"
        );
        assert!(
            graph
                .collections
                .keys()
                .any(|var| var.as_str() == "store.selected_id"),
            "store.selected_id should be compiled into the graph"
        );
    }

    #[test]
    fn crud_filter_pipeline_is_reactive_and_filters_rows() {
        let source = read_example("../../playground/frontend/src/examples/crud/crud.bn");

        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("crud should compile");

        let CompiledProgram::Dataflow { graph } = program else {
            panic!("expected CRUD to compile as dataflow");
        };

        let retain = graph
            .collections
            .values()
            .find_map(|spec| match spec {
                CollectionSpec::ListRetainReactive {
                    predicate,
                    filter_state,
                    ..
                } => Some((predicate, filter_state)),
                _ => None,
            })
            .unwrap_or_else(|| {
                let filter_vars: Vec<_> = graph
                    .collections
                    .keys()
                    .filter(|var| var.as_str().contains("filter_input"))
                    .map(|var| var.as_str().to_string())
                    .collect();
                panic!(
                    "CRUD should compile keyed retain as reactive; filter-related vars: {:?}",
                    filter_vars
                );
            });

        let hans = Value::object([
            ("name", Value::text("Hans")),
            ("surname", Value::text("Emil")),
        ]);
        let mustermann = Value::object([
            ("name", Value::text("Max")),
            ("surname", Value::text("Mustermann")),
        ]);

        assert!(
            !(retain.0)(&hans, &Value::text("M")),
            "filter M should exclude Hans/Emil"
        );
        assert!(
            (retain.0)(&mustermann, &Value::text("M")),
            "filter M should keep Mustermann"
        );
        assert!(
            matches!(
                graph.collections.get(retain.1),
                Some(CollectionSpec::HoldLatest(_))
            ),
            "reactive filter state should be wrapped in HoldLatest"
        );
    }

    #[test]
    fn crud_person_to_add_joins_create_press_with_text_inputs() {
        let source = read_example("../../playground/frontend/src/examples/crud/crud.bn");

        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("crud should compile");

        let CompiledProgram::Dataflow { graph } = program else {
            panic!("expected CRUD to compile as dataflow");
        };

        let person_to_add = graph
            .collections
            .get(&VarId::new("store.person_to_add"))
            .expect("store.person_to_add should exist");

        let CollectionSpec::Map { source, f } = person_to_add else {
            panic!("store.person_to_add should compile to a Map over joined deps");
        };

        let CollectionSpec::Join { .. } = graph
            .collections
            .get(source)
            .expect("store.person_to_add source should exist")
        else {
            panic!("store.person_to_add source should be a Join");
        };

        assert!(
            graph
                .collections
                .contains_key(&VarId::new("store.elements.name_input.text")),
            "name input text state should be synthesized as a reactive var"
        );
        assert!(
            graph
                .collections
                .contains_key(&VarId::new("store.elements.surname_input.text")),
            "surname input text state should be synthesized as a reactive var"
        );

        let mut combined_fields = BTreeMap::new();
        combined_fields.insert(Arc::from("__event"), Value::tag("Press"));
        combined_fields.insert(
            Arc::from(format!("{DEP_FIELD_PREFIX}0")),
            Value::text("John"),
        );
        combined_fields.insert(
            Arc::from(format!("{DEP_FIELD_PREFIX}1")),
            Value::text("Doe"),
        );
        let combined = Value::Object(Arc::new(combined_fields));
        let result = f(&combined);

        assert_eq!(
            result.get_field("name").and_then(|v| v.as_text()),
            Some("John"),
            "store.person_to_add should read current name input text"
        );
        assert_eq!(
            result.get_field("surname").and_then(|v| v.as_text()),
            Some("Doe"),
            "store.person_to_add should read current surname input text"
        );
        assert!(
            result.get_field("id").is_some(),
            "store.person_to_add should include generated id state"
        );
        assert!(
            result
                .get_field("person_elements")
                .and_then(|v| v.get_field("row"))
                .is_some(),
            "store.person_to_add should include row link state"
        );
    }

    #[test]
    fn crud_people_keyed_append_is_wired_to_person_to_add() {
        let source = read_example("../../playground/frontend/src/examples/crud/crud.bn");

        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("crud should compile");

        let CompiledProgram::Dataflow { graph } = program else {
            panic!("expected CRUD to compile as dataflow");
        };

        let append_var = graph
            .collections
            .iter()
            .find_map(|(var, spec)| match spec {
                CollectionSpec::AppendNewKeyed { source, .. }
                    if source == &VarId::new("store.person_to_add") =>
                {
                    Some(var.clone())
                }
                _ => None,
            })
            .expect("CRUD keyed append should consume store.person_to_add");

        let membership_uses_append = graph.collections.values().any(|spec| {
            matches!(
                spec,
                CollectionSpec::ListAppend { new_items, .. } if new_items == &append_var
            )
        });
        assert!(
            membership_uses_append,
            "CRUD keyed membership should consume the keyed append stream"
        );
    }

    #[test]
    fn crud_selected_id_uses_keyed_row_press_events() {
        let source = read_example("../../playground/frontend/src/examples/crud/crud.bn");

        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("crud should compile");

        let CompiledProgram::Dataflow { graph } = program else {
            panic!("expected CRUD to compile as dataflow");
        };

        let keyed_event_maps: Vec<String> = graph
            .collections
            .iter()
            .filter_map(|(var, spec)| match spec {
                CollectionSpec::KeyedEventMap { items, events, .. } => {
                    Some(format!("{} => items={}, events={}", var, items, events))
                }
                _ => None,
            })
            .collect();
        let selected_related: Vec<String> = graph
            .collections
            .iter()
            .filter(|(var, _)| var.as_str().contains("selected") || var.as_str().contains("latest"))
            .map(|(var, spec)| format!("{} => {:?}", var, std::mem::discriminant(spec)))
            .collect();

        assert!(
            graph
                .collections
                .values()
                .any(|spec| { matches!(spec, CollectionSpec::KeyedEventMap { .. }) }),
            "selected_id row-press pipeline should compile as a KeyedEventMap; keyed_event_maps={keyed_event_maps:?}; selected_related={selected_related:?}"
        );
    }

    #[test]
    fn crud_selected_id_wildcard_classifier_matches_row_press_path() {
        let source = read_example("../../playground/frontend/src/examples/crud/crud.bn");

        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("crud should compile");

        let CompiledProgram::Dataflow { graph } = program else {
            panic!("expected CRUD to compile as dataflow");
        };

        let events_var = graph
            .collections
            .values()
            .find_map(|spec| match spec {
                CollectionSpec::KeyedEventMap { events, .. } => Some(events.clone()),
                _ => None,
            })
            .expect("expected keyed event map for selected_id");

        let classify = match graph.collections.get(&events_var) {
            Some(CollectionSpec::MapToKeyed { classify, .. }) => classify,
            _ => panic!("expected MapToKeyed for selected_id events"),
        };

        let classified = classify(&Value::object([
            (
                "path",
                Value::text("store.people.0002.person_elements.row.event.press"),
            ),
            ("value", Value::tag("Press")),
        ]));

        let Some((key, payload)) = classified else {
            panic!("wildcard classifier did not match CRUD row press path");
        };
        assert_eq!(key.0.as_ref(), "0002");
        assert_eq!(payload.as_tag(), Some("Press"));
    }

    #[test]
    fn crud_selected_id_keyed_event_map_reads_item_id() {
        let source = read_example("../../playground/frontend/src/examples/crud/crud.bn");

        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("crud should compile");

        let CompiledProgram::Dataflow { graph } = program else {
            panic!("expected CRUD to compile as dataflow");
        };

        let transform = graph
            .collections
            .values()
            .find_map(|spec| match spec {
                CollectionSpec::KeyedEventMap { f, .. } => Some(f),
                _ => None,
            })
            .expect("expected keyed event map for selected_id");

        let item = Value::object([
            ("id", Value::text("person-42")),
            ("name", Value::text("Roman")),
            ("surname", Value::text("Tansen")),
        ]);

        let selected = transform(&item, &Value::tag("Press"));
        assert_eq!(selected.as_text(), Some("person-42"));
    }

    #[test]
    fn crud_display_pipeline_reacts_to_selected_id() {
        let source = read_example("../../playground/frontend/src/examples/crud/crud.bn");

        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("crud should compile");

        let CompiledProgram::Dataflow { graph } = program else {
            panic!("expected CRUD to compile as dataflow");
        };

        assert!(
            graph
                .collections
                .values()
                .any(|spec| { matches!(spec, CollectionSpec::ListMapWithKeyReactive { .. }) }),
            "CRUD display pipeline should compile as ListMapWithKeyReactive"
        );
    }

    #[test]
    fn crud_display_pipeline_uses_joined_dep_state() {
        let source = read_example("../../playground/frontend/src/examples/crud/crud.bn");

        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("crud should compile");

        let CompiledProgram::Dataflow { graph } = program else {
            panic!("expected CRUD to compile as dataflow");
        };

        let dep_var = graph
            .collections
            .values()
            .find_map(|spec| match spec {
                CollectionSpec::ListMapWithKeyReactive { dep, .. } => Some(dep.clone()),
                _ => None,
            })
            .expect("expected reactive display pipeline");

        assert!(
            matches!(
                graph.collections.get(&dep_var),
                Some(CollectionSpec::Join { .. })
            ),
            "reactive display dep should be a joined scalar object; dep_var={dep_var}"
        );
    }

    #[test]
    fn crud_delete_broadcast_handler_removes_selected_person() {
        let source = read_example("../../playground/frontend/src/examples/crud/crud.bn");

        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("crud should compile");

        let CompiledProgram::Dataflow { graph } = program else {
            panic!("expected CRUD to compile as dataflow");
        };

        let handler = graph
            .collections
            .values()
            .find_map(|spec| match spec {
                CollectionSpec::KeyedHoldState {
                    broadcast_handler: Some(handler),
                    ..
                } => Some(handler),
                _ => None,
            })
            .expect("expected CRUD keyed hold state with broadcast handler");
        let sample_event = graph
            .collections
            .values()
            .find_map(|spec| match spec {
                CollectionSpec::SampleOnEvent { f, .. } => Some(f),
                _ => None,
            })
            .expect("expected CRUD delete broadcast sample");

        let selected_id = Value::tagged("PersonId", [("id", Value::text("person-42"))]);
        let mut states = std::collections::HashMap::new();
        states.insert(
            ListKey::new("0001"),
            Value::object([
                (
                    "id",
                    Value::tagged("PersonId", [("id", Value::text("person-1"))]),
                ),
                ("surname", Value::text("Mustermann")),
            ]),
        );
        states.insert(
            ListKey::new("0002"),
            Value::object([
                ("id", selected_id.clone()),
                ("surname", Value::text("Tansen")),
            ]),
        );

        let delete_event = sample_event(&Value::tag("Press"), &selected_id);
        let removals = handler(&states, &delete_event);

        assert!(
            removals.contains(&(ListKey::new("0002"), None)),
            "expected CRUD delete broadcast to remove the selected row, got {removals:?}; event={delete_event}"
        );
        assert!(
            !removals.iter().any(|(key, _)| key.0.as_ref() == "0001"),
            "delete broadcast should not remove non-selected rows, got {removals:?}"
        );
    }

    #[test]
    fn counter_hold_compiles_state_self_reference_without_external_dep_join() {
        let source =
            read_example("../../playground/frontend/src/examples/counter_hold/counter_hold.bn");

        let program = compile(&source, None, &std::collections::HashMap::new(), None)
            .expect("counter_hold should compile");

        let CompiledProgram::Dataflow { graph } = program else {
            panic!("expected counter_hold to compile as dataflow");
        };

        assert!(
            graph.collections.contains_key(&VarId::new("counter")),
            "counter HOLD collection should exist"
        );
        assert!(
            graph.collections.contains_key(&graph.document),
            "document collection should exist for counter_hold"
        );
    }

    #[test]
    fn list_range_keys_preserve_numeric_iteration_order() {
        let source = r#"
numbers: List/range(from: 1, to: 12)

document: Document/new(root:
    Element/stripe(
        element: []
        direction: Row
        gap: 0
        style: []
        items: numbers |> List/map(old, new:
            Element/label(
                element: []
                style: []
                label: TEXT { {old} }
            )
        )
    )
)
"#;

        let program = compile(source, None, &std::collections::HashMap::new(), None)
            .expect("range document should compile");

        let CompiledProgram::Static { document_value, .. } = program else {
            panic!("expected static document");
        };

        let stripe_items = document_value
            .get_field("root")
            .and_then(|root| root.get_field("items"))
            .expect("stripe items");

        let labels: Vec<String> = stripe_items
            .list_items()
            .into_iter()
            .filter_map(|item| item.get_field("label"))
            .filter_map(|label| label.as_text().map(ToString::to_string))
            .collect();

        assert_eq!(
            labels,
            vec![
                "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12"
            ],
            "List/range labels should stay in numeric order"
        );
    }

    #[test]
    fn inject_item_link_paths_covers_generic_element_groups() {
        let item = Value::object([
            (
                "person_elements",
                Value::object([("row", Value::tag("LINK"))]),
            ),
            (
                "cell_elements",
                Value::object([("display", Value::tag("LINK"))]),
            ),
        ]);

        let injected = inject_item_link_paths_with_key(&item, "store.people", "0002");

        assert_eq!(
            injected
                .get_field("person_elements")
                .and_then(|v| v.get_field("row"))
                .and_then(|v| v.get_field(LINK_PATH_FIELD))
                .and_then(|v| v.as_text()),
            Some("store.people.0002.person_elements.row")
        );
        assert_eq!(
            injected
                .get_field("cell_elements")
                .and_then(|v| v.get_field("display"))
                .and_then(|v| v.get_field(LINK_PATH_FIELD))
                .and_then(|v| v.as_text()),
            Some("store.people.0002.cell_elements.display")
        );
    }

    #[test]
    fn static_list_map_latest_registers_item_event_inputs() {
        let source = r#"
items: LIST {
    [row: 1, cell_elements: [display: LINK]]
    [row: 2, cell_elements: [display: LINK]]
}
selected_row: items
    |> List/map(cell, new:
        cell.cell_elements.display.event.double_click |> THEN { cell.row }
    )
    |> List/latest()
document: Document/new(root:
    Element/label(element: [], style: [], label: selected_row)
)
"#;

        let program = compile(source, None, &std::collections::HashMap::new(), None)
            .expect("program should compile");
        let CompiledProgram::Dataflow { graph } = program else {
            panic!("expected dataflow");
        };

        let double_click_paths: Vec<String> = graph
            .inputs
            .iter()
            .filter(|input| input.kind == InputKind::DoubleClick)
            .filter_map(|input| input.link_path.clone())
            .collect();

        assert!(
            double_click_paths
                .iter()
                .any(|path| path == "items.0000.cell_elements.display.event.double_click"),
            "expected first static item double-click input path, got {double_click_paths:?}"
        );
        assert!(
            double_click_paths
                .iter()
                .any(|path| path == "items.0001.cell_elements.display.event.double_click"),
            "expected second static item double-click input path, got {double_click_paths:?}"
        );
    }

    #[test]
    fn decorate_reactive_scope_value_recurses_into_nested_lists() {
        let value = Value::tagged(
            LIST_TAG,
            [(
                "0001",
                Value::tagged(
                    LIST_TAG,
                    [(
                        "0001",
                        Value::object([(
                            "cell_elements",
                            Value::object([("display", Value::tag("LINK"))]),
                        )]),
                    )],
                ),
            )],
        );

        let decorated = decorate_reactive_scope_value(&value, "cells");

        assert_eq!(
            decorated
                .get_field("0001")
                .and_then(|v| v.get_field("0001"))
                .and_then(|v| v.get_field("cell_elements"))
                .and_then(|v| v.get_field("display"))
                .and_then(|v| v.get_field(LINK_PATH_FIELD))
                .and_then(|v| v.as_text()),
            Some("cells.0001.0001.cell_elements.display")
        );
    }
}
