use std::fmt;
use std::io::{Cursor, Read};

use ariadne::{Config, Label, Report, ReportKind, Source};
use zoon::{UnwrapThrowExt, eprintln, println};

/// Set to false to disable verbose source code and AST logging in console
const LOG_SOURCE_AND_AST: bool = false;

use crate::parser::{
    Input, ParseError, Parser, SourceCode, Spanned, Token, lexer, parser,
    reset_expression_depth, resolve_references, span_at, static_expression,
};
use super::{
    dd_evaluator::BoonDdRuntime,
    dd_reactive_eval::{DdReactiveContext, DdReactiveEvaluator},
    dd_value::DdValue,
};

/// Run a Boon program using the Differential Dataflow engine.
///
/// This parses the source code, evaluates it with the DD runtime, and returns
/// the document value.
pub fn run_dd(
    filename: &str,
    source_code: &str,
) -> Option<DdValue> {
    run_dd_with_injections(filename, source_code, std::collections::HashMap::new())
}

/// Re-evaluate a Boon program with an existing reactive context.
///
/// This re-parses and re-evaluates the program, but shares the existing
/// reactive context. When HOLDs are registered, `register_hold` returns
/// the existing signal (with its current value) instead of creating a new one.
/// This ensures derived values (like `active_todos_count`) see updated HOLD values.
///
/// Returns only the document - no new THEN connections are created since
/// they're already set up in the existing context.
pub fn run_dd_reactive_with_context(
    filename: &str,
    source_code: &str,
    ctx: DdReactiveContext,
) -> Option<DdValue> {
    // Create SourceCode for parsing
    let source_code_for_storage = source_code.to_string();
    let source_code_arc = SourceCode::new(source_code_for_storage.clone());
    let source_code = source_code_arc.as_str();

    // Lex
    let (tokens, errors) = lexer().parse(source_code).into_output_errors();
    if !errors.is_empty() {
        println!("[DD Re-eval Lex Errors]");
    }
    report_errors(errors, filename, source_code);
    let Some(mut tokens) = tokens else {
        return None;
    };

    tokens.retain(|spanned_token| !matches!(spanned_token.node, Token::Comment(_)));

    // Parse
    reset_expression_depth();
    let (ast, errors) = parser()
        .parse(tokens.map(
            span_at(source_code.len()),
            |Spanned {
                 node,
                 span,
                 persistence: _,
             }| { (node, span) },
        ))
        .into_output_errors();
    if !errors.is_empty() {
        println!("[DD Re-eval Parse Errors]");
    }
    report_errors(errors, filename, source_code);
    let Some(ast) = ast else {
        return None;
    };

    // Resolve references
    let ast = match resolve_references(ast) {
        Ok(ast) => ast,
        Err(errors) => {
            println!("[DD Re-eval Reference Errors]");
            report_errors(errors, filename, source_code);
            return None;
        }
    };

    // Convert to static expressions
    let static_ast = static_expression::convert_expressions(source_code_arc.clone(), ast);

    // Start a new evaluation cycle - this clears active prefix tracking
    // so we can detect which items were removed during re-evaluation
    ctx.start_evaluation_cycle();

    // Evaluate with reactive evaluator using the existing context
    let mut evaluator = DdReactiveEvaluator::new_with_context(ctx);
    evaluator.evaluate(&static_ast);

    // NOTE: cleanup_orphaned_holds() is DISABLED for now because it incorrectly
    // removes HOLDs for items that are just filtered out (not rendered) but still
    // exist in the data. This causes state loss when switching between filters.
    // TODO: Implement smarter cleanup that distinguishes between:
    // 1. Items temporarily hidden by filter (keep HOLDs)
    // 2. Items permanently deleted (clean up HOLDs)
    // evaluator.reactive_context().cleanup_orphaned_holds();

    // Return only the document - THEN connections are already set up
    evaluator.get_document().cloned()
}

/// Run a Boon program with injected variables.
///
/// Injected variables override AST-defined variables. This allows external
/// state (e.g., from ReactiveContext) to be passed into the evaluation.
pub fn run_dd_with_injections(
    filename: &str,
    source_code: &str,
    injected_vars: std::collections::HashMap<String, DdValue>,
) -> Option<DdValue> {
    // Create SourceCode for parsing
    let source_code_for_storage = source_code.to_string();
    let source_code_arc = SourceCode::new(source_code_for_storage.clone());
    let source_code = source_code_arc.as_str();

    if LOG_SOURCE_AND_AST {
        println!("[DD Source Code ({filename})]");
        println!("{source_code}");
    }

    // Lex
    let (tokens, errors) = lexer().parse(source_code).into_output_errors();
    if !errors.is_empty() {
        println!("[DD Lex Errors]");
    }
    report_errors(errors, filename, source_code);
    let Some(mut tokens) = tokens else {
        return None;
    };

    tokens.retain(|spanned_token| !matches!(spanned_token.node, Token::Comment(_)));

    // Parse
    reset_expression_depth();
    let (ast, errors) = parser()
        .parse(tokens.map(
            span_at(source_code.len()),
            |Spanned {
                 node,
                 span,
                 persistence: _,
             }| { (node, span) },
        ))
        .into_output_errors();
    if !errors.is_empty() {
        println!("[DD Parse Errors]");
    }
    report_errors(errors, filename, source_code);
    let Some(ast) = ast else {
        return None;
    };

    // Resolve references
    let ast = match resolve_references(ast) {
        Ok(ast) => ast,
        Err(errors) => {
            println!("[DD Reference Errors]");
            report_errors(errors, filename, source_code);
            return None;
        }
    };

    // Convert to static expressions (owned, 'static)
    let static_ast = static_expression::convert_expressions(source_code_arc.clone(), ast);

    if LOG_SOURCE_AND_AST {
        println!("[DD Static AST]");
        println!("{static_ast:#?}");
    }

    // Evaluate with DD runtime
    let mut runtime = BoonDdRuntime::new();

    // Inject any pre-set variables (these won't be overwritten by AST evaluation)
    runtime.inject_variables(injected_vars);

    runtime.evaluate(&static_ast);

    // Return the document output
    runtime.get_document().cloned()
}

/// Result of reactive DD evaluation.
pub struct DdReactiveResult {
    pub document: Option<DdValue>,
    pub context: DdReactiveContext,
    /// Source code for re-evaluation on render
    pub source_code: String,
    /// Filename for re-evaluation
    pub filename: String,
    /// Storage prefix for persistence
    pub storage_prefix: Option<String>,
}

/// Run a Boon program with reactive evaluation.
///
/// Returns the document value and the reactive context for event wiring.
pub fn run_dd_reactive(
    filename: &str,
    source_code: &str,
) -> Option<DdReactiveResult> {
    run_dd_reactive_with_persistence(filename, source_code, None)
}

/// Run a Boon program with reactive evaluation and optional persistence.
pub fn run_dd_reactive_with_persistence(
    filename: &str,
    source_code: &str,
    storage_prefix: Option<&str>,
) -> Option<DdReactiveResult> {
    // Create SourceCode for parsing
    let source_code_for_storage = source_code.to_string();
    let source_code_arc = SourceCode::new(source_code_for_storage.clone());
    let source_code = source_code_arc.as_str();

    if LOG_SOURCE_AND_AST {
        println!("[DD Reactive Source Code ({filename})]");
        println!("{source_code}");
    }

    // Lex
    let (tokens, errors) = lexer().parse(source_code).into_output_errors();
    if !errors.is_empty() {
        println!("[DD Lex Errors]");
    }
    report_errors(errors, filename, source_code);
    let Some(mut tokens) = tokens else {
        return None;
    };

    tokens.retain(|spanned_token| !matches!(spanned_token.node, Token::Comment(_)));

    // Parse
    reset_expression_depth();
    let (ast, errors) = parser()
        .parse(tokens.map(
            span_at(source_code.len()),
            |Spanned {
                 node,
                 span,
                 persistence: _,
             }| { (node, span) },
        ))
        .into_output_errors();
    if !errors.is_empty() {
        println!("[DD Parse Errors]");
    }
    report_errors(errors, filename, source_code);
    let Some(ast) = ast else {
        return None;
    };

    // Resolve references
    let ast = match resolve_references(ast) {
        Ok(ast) => ast,
        Err(errors) => {
            println!("[DD Reference Errors]");
            report_errors(errors, filename, source_code);
            return None;
        }
    };

    // Convert to static expressions
    let static_ast = static_expression::convert_expressions(source_code_arc.clone(), ast);

    if LOG_SOURCE_AND_AST {
        println!("[DD Reactive Static AST]");
        println!("{static_ast:#?}");
    }

    // Evaluate with reactive evaluator
    let mut evaluator = if let Some(prefix) = storage_prefix {
        DdReactiveEvaluator::new_with_persistence(prefix)
    } else {
        DdReactiveEvaluator::new()
    };

    evaluator.evaluate(&static_ast);

    let document = evaluator.get_document().cloned();
    let (context, then_connections) = evaluator.into_reactive_context_with_connections();

    // Debug: Log how many THEN connections were created
    zoon::println!("[DD Interpreter] Created {} THEN connections", then_connections.len());
    for (i, conn) in then_connections.iter().enumerate() {
        zoon::println!("[DD Interpreter]   conn[{}] trigger={:?} hold_id={:?}", i, conn.trigger, conn.hold_id);
    }

    // Wire up the THEN connections to the reactive context
    context.set_then_connections(then_connections);

    Some(DdReactiveResult {
        document,
        context,
        source_code: source_code.to_string(),
        filename: filename.to_string(),
        storage_prefix: storage_prefix.map(|s| s.to_string()),
    })
}

fn report_errors<'code, T: fmt::Display + 'code>(
    errors: impl IntoIterator<Item = ParseError<'code, T>>,
    filename: &str,
    source_code: &str,
) {
    let mut report_bytes = Cursor::new(Vec::new());
    let mut report_string = String::new();
    for error in errors {
        report_bytes.set_position(0);
        report_bytes.get_mut().clear();
        Report::build(ReportKind::Error, (filename, error.span().into_range()))
            .with_config(Config::default().with_color(false))
            .with_message(error.to_string())
            .with_label(
                Label::new((filename, error.span().into_range()))
                    .with_message(error.reason().to_string()),
            )
            .finish()
            .write((filename, Source::from(source_code)), &mut report_bytes)
            .unwrap_throw();
        report_bytes.set_position(0);
        report_string.clear();
        report_bytes
            .read_to_string(&mut report_string)
            .unwrap_throw();
        eprintln!("{report_string}");
    }
}
