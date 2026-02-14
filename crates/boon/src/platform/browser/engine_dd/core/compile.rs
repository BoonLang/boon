//! Boon source → compiled program.
//!
//! Parses Boon source code and compiles it into a `CompiledProgram`:
//! - Static programs (no LINK/HOLD/Timer) → evaluated to a Value
//! - Reactive programs → DD dataflow spec with link bindings
//!
//! Uses the existing Boon parser. The compiler walks the static AST
//! and evaluates/compiles expressions.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::parser::{
    lexer, parser, reset_expression_depth, resolve_references, span_at,
    static_expression::{
        self, Alias, Argument, ArithmeticOperator, Expression, Literal, Spanned, TextPart,
    },
    SourceCode,
};

use super::types::{InputId, LinkId};
use super::value::Value;
/// Result of compiling a Boon program.
pub enum CompiledProgram {
    /// Purely static program — no reactive computation.
    Static { document_value: Value },

    /// Reactive program with a single HOLD state variable.
    ///
    /// The DD dataflow manages the HOLD state. The `build_document` closure
    /// takes the current state value and produces the full document Value tree.
    SingleHold {
        initial_value: Value,
        hold_transform: Arc<dyn Fn(&Value, &Value) -> Value>,
        build_document: Arc<dyn Fn(&Value) -> Value>,
        link_bindings: Vec<LinkBinding>,
    },

    /// Reactive program with LATEST + running sum.
    LatestSum {
        build_document: Arc<dyn Fn(&Value) -> Value>,
        link_bindings: Vec<LinkBinding>,
    },

    /// General reactive program — handles any Boon program.
    ///
    /// Stores the full AST for interpretation by the general reactive evaluator.
    General {
        variables: Vec<(String, Spanned<Expression>)>,
        functions: Vec<(String, Vec<String>, Spanned<Expression>)>,
    },
}

/// A binding from a LINK path to a DD input.
pub struct LinkBinding {
    pub link_id: LinkId,
    pub input_id: InputId,
}

/// Compile Boon source code into a program.
pub fn compile(source_code: &str) -> Result<CompiledProgram, String> {
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
        // Try specialized compilation first
        match compiler.compile_reactive(&doc_expr) {
            Ok(program) => Ok(program),
            Err(_) => {
                // Fall back to general interpreter
                Ok(CompiledProgram::General {
                    variables: compiler.variables.clone(),
                    functions: compiler.functions.clone(),
                })
            }
        }
    } else {
        // Try static evaluation
        match compiler.eval_static(&doc_expr) {
            Ok(value) => Ok(CompiledProgram::Static {
                document_value: value,
            }),
            Err(_) => {
                // Static eval failed (e.g., fibonacci with HOLD/WHILE)
                // Use general interpreter
                Ok(CompiledProgram::General {
                    variables: compiler.variables.clone(),
                    functions: compiler.functions.clone(),
                })
            }
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
        self.variables.iter().any(|(_, expr)| Self::is_reactive(expr))
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
            Expression::FunctionCall { arguments, .. } => {
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
                } => Self::is_reactive(operand_a) || Self::is_reactive(operand_b),
                ArithmeticOperator::Negate { operand } => Self::is_reactive(operand),
            },
            Expression::TextLiteral { parts } => parts.iter().any(|p| matches!(p, TextPart::Interpolation { .. })),
            Expression::Alias(_) => {
                // An alias might reference a reactive variable — conservative.
                // But for top-level reactivity check, we look at definitions.
                false
            }
            _ => false,
        }
    }

    // -----------------------------------------------------------------------
    // Static evaluation (non-reactive programs)
    // -----------------------------------------------------------------------

    fn eval_static(&self, expr: &Spanned<Expression>) -> Result<Value, String> {
        self.eval_static_with_scope(expr, &[])
    }

    fn eval_static_with_scope(
        &self,
        expr: &Spanned<Expression>,
        local_scope: &[(&str, Value)],
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
                            .ok_or_else(|| format!("Field '{}' not found", part.as_str()))?;
                    }
                    Ok(val)
                }
                Alias::WithPassed { .. } => {
                    Err("PASSED not supported in static evaluation".to_string())
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
                let mut new_scope: Vec<(&str, Value)> = local_scope.to_vec();
                for var in variables {
                    let val = self.eval_static_with_scope(&var.node.value, &new_scope)?;
                    // Leak the name to get a &'static str - this is fine for compilation
                    let name_str = var.node.name.as_str();
                    // Use a small trick: find if name already exists in scope
                    let already = new_scope.iter().position(|(n, _)| *n == name_str);
                    if let Some(idx) = already {
                        new_scope[idx].1 = val;
                    } else {
                        // We need a stable reference - push raw pointer
                        let name_owned: String = name_str.to_string();
                        let name_leaked: &str =
                            unsafe { &*(name_owned.as_str() as *const str) };
                        std::mem::forget(name_owned);
                        new_scope.push((name_leaked, val));
                    }
                }
                self.eval_static_with_scope(output, &new_scope)
            }

            Expression::ArithmeticOperator(op) => self.eval_arithmetic_static(op, local_scope),

            Expression::Comparator(cmp) => self.eval_comparator_static(cmp, local_scope),

            Expression::When { arms } => {
                // Static WHEN: just evaluate the first matching arm
                // (pipe input not available in static context, so this is limited)
                Err("WHEN requires pipe input in static context".to_string())
            }

            Expression::Skip => Ok(Value::Unit),

            // LINK in static context becomes a marker
            Expression::Link => Ok(Value::tag("LINK")),

            _ => Err(format!("Unsupported expression in static eval: {:?}", std::mem::discriminant(&expr.node))),
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
        local_scope: &[(&str, Value)],
    ) -> Result<Value, String> {
        // Check local scope first
        for (n, v) in local_scope.iter().rev() {
            if *n == name {
                return Ok(v.clone());
            }
        }
        // Check top-level variables
        if let Some(expr) = self.get_var_expr(name) {
            return self.eval_static_with_scope(expr, local_scope);
        }
        Err(format!("Variable '{}' not found", name))
    }

    fn eval_function_call_static(
        &self,
        path: &[crate::parser::StrSlice],
        arguments: &[Spanned<Argument>],
        local_scope: &[(&str, Value)],
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
                // Unwrap: Document/new(root: X) returns X wrapped as DocumentNew
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
                // In pipe context, this would accumulate, but statically just pass through
                Ok(Value::number(0.0))
            }

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
        local_scope: &[(&str, Value)],
    ) -> Result<Value, String> {
        let mut fields = BTreeMap::new();
        for arg in arguments {
            let name = arg.node.name.as_str();
            if let Some(ref val_expr) = arg.node.value {
                let val = self.eval_static_with_scope(val_expr, local_scope)?;
                // Handle LINK in element argument: [event: [press: LINK]]
                // For static eval, just record the link path
                fields.insert(Arc::from(name), val);
            }
        }
        Ok(Value::Tagged {
            tag: Arc::from(tag),
            fields: Arc::new(fields),
        })
    }

    fn eval_user_function_static(
        &self,
        fn_name: &str,
        arguments: &[Spanned<Argument>],
        local_scope: &[(&str, Value)],
    ) -> Result<Value, String> {
        // Find the function definition
        let func = self
            .functions
            .iter()
            .find(|(name, _, _)| name == fn_name)
            .ok_or_else(|| format!("Function '{}' not found", fn_name))?
            .clone();

        let (_, params, body) = func;

        // Build function scope from arguments
        let mut fn_scope: Vec<(&str, Value)> = local_scope.to_vec();
        for arg in arguments {
            let arg_name = arg.node.name.as_str();
            if let Some(ref val_expr) = arg.node.value {
                let val = self.eval_static_with_scope(val_expr, local_scope)?;
                let name_owned: String = arg_name.to_string();
                let name_leaked: &str = unsafe { &*(name_owned.as_str() as *const str) };
                std::mem::forget(name_owned);
                fn_scope.push((name_leaked, val));
            }
        }

        // Also bind positional params
        for (i, param_name) in params.iter().enumerate() {
            if i < arguments.len() {
                if let Some(ref val_expr) = arguments[i].node.value {
                    let val = self.eval_static_with_scope(val_expr, local_scope)?;
                    let name_owned: String = param_name.clone();
                    let name_leaked: &str = unsafe { &*(name_owned.as_str() as *const str) };
                    std::mem::forget(name_owned);
                    fn_scope.push((name_leaked, val));
                }
            }
        }

        self.eval_static_with_scope(&body, &fn_scope)
    }

    fn eval_pipe_static(
        &self,
        from: &Spanned<Expression>,
        to: &Spanned<Expression>,
        local_scope: &[(&str, Value)],
    ) -> Result<Value, String> {
        // Evaluate the left side
        let from_val = self.eval_static_with_scope(from, local_scope)?;

        // The right side is typically a FunctionCall that receives `from_val` as implicit argument
        match &to.node {
            Expression::FunctionCall { path, arguments } => {
                let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                match path_strs.as_slice() {
                    ["Document", "new"] => {
                        // pipe |> Document/new() — from_val is the root
                        // But Document/new usually takes root: argument
                        if arguments.is_empty() {
                            Ok(Value::tagged("DocumentNew", [("root", from_val)]))
                        } else {
                            self.eval_function_call_static(path, arguments, local_scope)
                        }
                    }
                    _ => {
                        // Generic pipe: evaluate as function call
                        // The piped value might be used implicitly
                        self.eval_function_call_static(path, arguments, local_scope)
                    }
                }
            }
            Expression::FieldAccess { path: field_path } => {
                // .field access on piped value
                let mut val = from_val;
                for field in field_path {
                    val = val
                        .get_field(field.as_str())
                        .cloned()
                        .ok_or_else(|| format!("Field '{}' not found", field.as_str()))?;
                }
                Ok(val)
            }
            _ => {
                // Evaluate 'to' and ignore 'from' for now
                self.eval_static_with_scope(to, local_scope)
            }
        }
    }

    fn eval_arithmetic_static(
        &self,
        op: &ArithmeticOperator,
        local_scope: &[(&str, Value)],
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
        local_scope: &[(&str, Value)],
    ) -> Result<Value, String> {
        use static_expression::Comparator;
        match cmp {
            Comparator::Equal {
                operand_a,
                operand_b,
            } => {
                let a = self.eval_static_with_scope(operand_a, local_scope)?;
                let b = self.eval_static_with_scope(operand_b, local_scope)?;
                Ok(Value::bool(a == b))
            }
            Comparator::NotEqual {
                operand_a,
                operand_b,
            } => {
                let a = self.eval_static_with_scope(operand_a, local_scope)?;
                let b = self.eval_static_with_scope(operand_b, local_scope)?;
                Ok(Value::bool(a != b))
            }
            Comparator::Greater {
                operand_a,
                operand_b,
            } => {
                let a = self.eval_static_with_scope(operand_a, local_scope)?;
                let b = self.eval_static_with_scope(operand_b, local_scope)?;
                Ok(Value::bool(a > b))
            }
            Comparator::GreaterOrEqual {
                operand_a,
                operand_b,
            } => {
                let a = self.eval_static_with_scope(operand_a, local_scope)?;
                let b = self.eval_static_with_scope(operand_b, local_scope)?;
                Ok(Value::bool(a >= b))
            }
            Comparator::Less {
                operand_a,
                operand_b,
            } => {
                let a = self.eval_static_with_scope(operand_a, local_scope)?;
                let b = self.eval_static_with_scope(operand_b, local_scope)?;
                Ok(Value::bool(a < b))
            }
            Comparator::LessOrEqual {
                operand_a,
                operand_b,
            } => {
                let a = self.eval_static_with_scope(operand_a, local_scope)?;
                let b = self.eval_static_with_scope(operand_b, local_scope)?;
                Ok(Value::bool(a <= b))
            }
        }
    }

    // -----------------------------------------------------------------------
    // Reactive compilation
    // -----------------------------------------------------------------------

    fn compile_reactive(
        &self,
        doc_expr: &Spanned<Expression>,
    ) -> Result<CompiledProgram, String> {
        // Identify the reactive variable (the one with HOLD or LATEST)
        let reactive_var = self.find_reactive_var()?;

        match &reactive_var.pattern {
            ReactivePattern::Hold {
                state_name,
                initial_value,
                hold_transform,
                link_id,
            } => {
                let link_bindings = vec![LinkBinding {
                    link_id: link_id.clone(),
                    input_id: InputId(0),
                }];

                let initial = initial_value.clone();
                let transform = hold_transform.clone();

                // Build the document closure that takes the reactive value
                // and produces the full document Value tree
                let doc_builder = self.build_document_closure(
                    &reactive_var.name,
                    doc_expr,
                    &link_bindings,
                )?;

                Ok(CompiledProgram::SingleHold {
                    initial_value: initial,
                    hold_transform: transform,
                    build_document: doc_builder,
                    link_bindings,
                })
            }
            ReactivePattern::LatestSum { link_id } => {
                let link_bindings = vec![LinkBinding {
                    link_id: link_id.clone(),
                    input_id: InputId(0),
                }];

                let doc_builder = self.build_document_closure(
                    &reactive_var.name,
                    doc_expr,
                    &link_bindings,
                )?;

                Ok(CompiledProgram::LatestSum {
                    build_document: doc_builder,
                    link_bindings,
                })
            }
        }
    }

    fn find_reactive_var(&self) -> Result<ReactiveVar, String> {
        for (name, expr) in &self.variables {
            if let Some(pattern) = self.analyze_reactive_pattern(name, expr) {
                return Ok(ReactiveVar {
                    name: name.clone(),
                    pattern,
                });
            }
        }
        Err("No reactive variable found".to_string())
    }

    fn analyze_reactive_pattern(
        &self,
        var_name: &str,
        expr: &Spanned<Expression>,
    ) -> Option<ReactivePattern> {
        match &expr.node {
            // Pattern: `initial |> HOLD state { body }`
            Expression::Pipe { from, to } => match &to.node {
                Expression::Hold { state_param, body } => {
                    // Evaluate initial value statically
                    let initial = self.eval_static(from).ok()?;
                    let state_name = state_param.as_str().to_string();

                    // Find LINK in the body
                    let link_id = self.find_link_in_expr(body)?;

                    // Build the hold transform closure
                    let transform = self.build_hold_transform(&state_name, body);

                    Some(ReactivePattern::Hold {
                        state_name,
                        initial_value: initial,
                        hold_transform: transform,
                        link_id,
                    })
                }
                _ => {
                    // Check if `from` is reactive
                    self.analyze_reactive_pattern(var_name, from)
                        .or_else(|| self.analyze_reactive_pattern(var_name, to))
                }
            },

            // Pattern: `LATEST { ... } |> Math/sum()` or just `LATEST { ... }`
            Expression::Latest { inputs } => {
                // Find LINK in inputs
                let link_id = inputs.iter().find_map(|i| self.find_link_in_expr(i))?;
                Some(ReactivePattern::LatestSum { link_id })
            }

            _ => None,
        }
    }

    fn find_link_in_expr(&self, expr: &Spanned<Expression>) -> Option<LinkId> {
        match &expr.node {
            Expression::Link => None, // Bare LINK has no path
            Expression::Pipe { from, to } => {
                self.find_link_in_expr(from).or_else(|| self.find_link_in_expr(to))
            }
            Expression::Then { body } => self.find_link_in_expr(body),
            Expression::Alias(Alias::WithoutPassed { parts, .. }) => {
                // e.g., increment_button.event.press — check if it references a var with LINK
                let var_name = parts[0].as_str();
                if let Some(var_expr) = self.get_var_expr(var_name) {
                    // Check if this variable has a LINK in its element definition
                    let link_path = self.find_link_path_in_element(var_name, var_expr);
                    if link_path.is_some() {
                        // Build LinkId from the alias path
                        let path_str: String = parts
                            .iter()
                            .map(|p| p.as_str())
                            .collect::<Vec<_>>()
                            .join(".");
                        return Some(LinkId::new(path_str));
                    }
                }
                None
            }
            Expression::Latest { inputs } => {
                inputs.iter().find_map(|i| self.find_link_in_expr(i))
            }
            Expression::Hold { body, .. } => self.find_link_in_expr(body),
            _ => None,
        }
    }

    fn find_link_path_in_element(
        &self,
        _var_name: &str,
        expr: &Spanned<Expression>,
    ) -> Option<String> {
        match &expr.node {
            Expression::FunctionCall { path, arguments } => {
                let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                if path_strs.first() == Some(&"Element") {
                    // Check if any argument contains LINK
                    for arg in arguments {
                        if let Some(ref val) = arg.node.value {
                            if self.expr_contains_link(val) {
                                return Some("press".to_string());
                            }
                        }
                    }
                }
                None
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
            _ => false,
        }
    }

    fn build_hold_transform(
        &self,
        state_name: &str,
        body: &Spanned<Expression>,
    ) -> Arc<dyn Fn(&Value, &Value) -> Value> {
        // For counter_hold pattern: body is `event_source |> THEN { state + 1 }`
        // The transform is: (state, _event) -> state + 1
        //
        // We need to analyze the THEN body to extract the transform.
        // For now, detect the common pattern: `state_name + N`
        let transform = self.extract_hold_body_transform(state_name, body);
        match transform {
            HoldTransform::Increment(n) => Arc::new(move |state: &Value, _event: &Value| {
                let current = state.as_number().unwrap_or(0.0);
                Value::number(current + n)
            }),
            HoldTransform::Custom => {
                // Fallback: increment by 1
                Arc::new(|state: &Value, _event: &Value| {
                    let current = state.as_number().unwrap_or(0.0);
                    Value::number(current + 1.0)
                })
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
                // Check if one side is state_name and other is a literal
                if self.is_alias_named(&operand_a.node, state_name) {
                    if let Expression::Literal(Literal::Number(n)) = &operand_b.node {
                        return HoldTransform::Increment(*n);
                    }
                }
                if self.is_alias_named(&operand_b.node, state_name) {
                    if let Expression::Literal(Literal::Number(n)) = &operand_a.node {
                        return HoldTransform::Increment(*n);
                    }
                }
                HoldTransform::Custom
            }
            _ => HoldTransform::Custom,
        }
    }

    fn is_alias_named(&self, expr: &Expression, name: &str) -> bool {
        match expr {
            Expression::Alias(Alias::WithoutPassed { parts, .. }) => {
                parts.len() == 1 && parts[0].as_str() == name
            }
            _ => false,
        }
    }

    fn build_document_closure(
        &self,
        reactive_var_name: &str,
        doc_expr: &Spanned<Expression>,
        link_bindings: &[LinkBinding],
    ) -> Result<Arc<dyn Fn(&Value) -> Value>, String> {
        // Capture all the static context needed to build the document
        // The closure takes the reactive value and produces the full document Value
        let doc_template = self.build_doc_template(reactive_var_name, doc_expr, link_bindings)?;

        Ok(Arc::new(move |reactive_value: &Value| {
            doc_template.instantiate(reactive_value)
        }))
    }

    fn build_doc_template(
        &self,
        reactive_var_name: &str,
        expr: &Spanned<Expression>,
        link_bindings: &[LinkBinding],
    ) -> Result<DocTemplate, String> {
        match &expr.node {
            Expression::Alias(Alias::WithoutPassed { parts, .. }) => {
                let name = parts[0].as_str();
                if name == reactive_var_name && parts.len() == 1 {
                    Ok(DocTemplate::ReactiveRef)
                } else if parts.len() == 1 {
                    // Look up variable definition and build template from it
                    if let Some(var_expr) = self.get_var_expr(name).cloned() {
                        self.build_doc_template(reactive_var_name, &var_expr, link_bindings)
                    } else {
                        let val = self.eval_static(expr)?;
                        Ok(DocTemplate::Static(val))
                    }
                } else {
                    // Multi-part alias (field access) - evaluate statically
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
                                let tmpl = self.build_doc_template(
                                    reactive_var_name,
                                    val_expr,
                                    link_bindings,
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
                                let tmpl = self.build_doc_template(
                                    reactive_var_name,
                                    val_expr,
                                    link_bindings,
                                )?;
                                // Check for LINK binding and add press_link
                                if name == "element" {
                                    if let Some(link_binding) = self.check_link_in_element(
                                        val_expr,
                                        link_bindings,
                                    ) {
                                        field_templates.push((
                                            "press_link".to_string(),
                                            DocTemplate::Static(Value::text(
                                                link_binding.link_id.as_str(),
                                            )),
                                        ));
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
                        // Try static evaluation
                        let val = self.eval_static(expr)?;
                        Ok(DocTemplate::Static(val))
                    }
                }
            }

            Expression::Pipe { from, to } => {
                // Handle pipe: from |> to
                match &to.node {
                    Expression::FunctionCall { .. } => {
                        self.build_doc_template(reactive_var_name, to, link_bindings)
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
                    let tmpl =
                        self.build_doc_template(reactive_var_name, item, link_bindings)?;
                    item_templates.push((format!("{:04}", i), tmpl));
                }
                Ok(DocTemplate::Tagged {
                    tag: "List".to_string(),
                    fields: item_templates,
                })
            }

            Expression::TextLiteral { parts } => {
                // Check if any part references the reactive variable
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
                                    // Try to evaluate statically
                                    let val = self
                                        .resolve_alias_static(var.as_str(), &[])
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
                // Build template for each field (might contain LINKs or reactive refs)
                let mut field_templates = Vec::new();
                for var in &obj.variables {
                    let name = var.node.name.as_str().to_string();
                    let tmpl = self.build_doc_template(
                        reactive_var_name,
                        &var.node.value,
                        link_bindings,
                    )?;
                    field_templates.push((name, tmpl));
                }
                // Wrap as an unnamed tagged value (Object renders as Tagged with empty tag)
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
                // Try static evaluation as fallback
                match self.eval_static(expr) {
                    Ok(val) => Ok(DocTemplate::Static(val)),
                    Err(e) => Err(format!("Cannot build doc template: {}", e)),
                }
            }
        }
    }

    fn check_link_in_element<'a>(
        &self,
        element_expr: &Spanned<Expression>,
        link_bindings: &'a [LinkBinding],
    ) -> Option<&'a LinkBinding> {
        if self.expr_contains_link(element_expr) {
            link_bindings.first()
        } else {
            None
        }
    }
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

// ---------------------------------------------------------------------------
// Internal types
// ---------------------------------------------------------------------------

struct ReactiveVar {
    name: String,
    pattern: ReactivePattern,
}

enum ReactivePattern {
    Hold {
        state_name: String,
        initial_value: Value,
        hold_transform: Arc<dyn Fn(&Value, &Value) -> Value>,
        link_id: LinkId,
    },
    LatestSum {
        link_id: LinkId,
    },
}

enum HoldTransform {
    Increment(f64),
    Custom,
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().to_string() + c.as_str(),
    }
}
