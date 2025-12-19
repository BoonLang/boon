use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::io::{Cursor, Read};
use std::sync::Arc;

use ariadne::{Config, Label, Report, ReportKind, Source};
use chumsky::input::Stream;
use serde_json_any_key::MapIterToJson;
use zoon::{UnwrapThrowExt, WebStorage, eprintln, local_storage, println, serde_json};

/// Set to false to disable verbose source code and AST logging in console
const LOG_SOURCE_AND_AST: bool = false;

use crate::parser::{
    Expression, Input, ParseError, Parser, SourceCode, Spanned, Token, lexer, parser,
    reset_expression_depth, resolve_persistence, resolve_references, span_at, static_expression,
};
use crate::platform::browser::{
    engine::{ConstructContext, LinkConnector, LinkVariableConnector, Object, PassThroughConnector, ReferenceConnector, VirtualFilesystem},
    evaluator::{evaluate_with_registry, FunctionRegistry, ModuleLoader},
};

/// Run a Boon program and return the result.
///
/// IMPORTANT: The returned `ReferenceConnector` and `LinkConnector` MUST be dropped
/// when the program is finished (e.g., when switching examples) to allow all actors
/// to be cleaned up. These connectors hold references to all top-level actors.
pub fn run(
    filename: &str,
    source_code: &str,
    states_local_storage_key: impl Into<Cow<'static, str>>,
    old_code_local_storage_key: impl Into<Cow<'static, str>>,
    old_span_id_pairs_local_storage_key: impl Into<Cow<'static, str>>,
    virtual_fs: VirtualFilesystem,
) -> Option<(Arc<Object>, ConstructContext, Arc<ReferenceConnector>, Arc<LinkConnector>, Arc<PassThroughConnector>, Arc<LinkVariableConnector>)> {
    println!("*** BOON INTERPRETER VERSION 2025-12-08-A ***");
    let states_local_storage_key = states_local_storage_key.into();
    let old_code_local_storage_key = old_code_local_storage_key.into();
    let old_span_id_pairs_local_storage_key = old_span_id_pairs_local_storage_key.into();

    // Create SourceCode FIRST so all parsing borrows from this Arc'd String.
    // This is critical: the AST will contain &str slices that point into this allocation.
    // If we create SourceCode after parsing, the pointers won't match.
    let source_code_for_storage = source_code.to_string();
    let source_code_arc = SourceCode::new(source_code_for_storage.clone());
    let source_code = source_code_arc.as_str();

    let old_source_code = local_storage().get::<String>(&old_code_local_storage_key);
    let old_ast = if let Some(Ok(old_source_code)) = &old_source_code {
        parse_old(filename, old_source_code)
    } else {
        None
    };

    if LOG_SOURCE_AND_AST {
        println!("[Source Code ({filename})]");
        println!("{source_code}");
    }

    let (tokens, errors) = lexer().parse(source_code).into_output_errors();
    if let Some(tokens) = tokens.as_ref() {
        // println!("[Tokens]");
        // println!("{tokens:?}");
    }
    if !errors.is_empty() {
        println!("[Lex Errors]");
    }
    report_errors(errors, filename, source_code);
    let Some(mut tokens) = tokens else {
        return None;
    };

    tokens.retain(|spanned_token| !matches!(spanned_token.node, Token::Comment(_)));

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
    if let Some(ast) = ast.as_ref() {
        // println!("[Abstract Syntax Tree]");
        // println!("{ast:?}");
    }
    if !errors.is_empty() {
        println!("[Parse Errors]");
    }
    report_errors(errors, filename, source_code);
    let Some(ast) = ast else {
        return None;
    };

    let ast = match resolve_references(ast) {
        Ok(ast) => ast,
        Err(errors) => {
            println!("[Reference Errors]");
            report_errors(errors, filename, source_code);
            return None;
        }
    };
    // println!("[Abstract Syntax Tree with Reference Data]");
    // println!("{ast:?}");

    let (ast, new_span_id_pairs) =
        match resolve_persistence(ast, old_ast, &old_span_id_pairs_local_storage_key) {
            Ok(ast) => ast,
            Err(errors) => {
                println!("[Persistence Errors]");
                report_errors(errors, filename, source_code);
                return None;
            }
        };
    if LOG_SOURCE_AND_AST {
        println!("[Abstract Syntax Tree with Reference Data and Persistence]");
        println!("{ast:#?}");
    }

    // Convert to static expressions (owned, 'static, no lifetimes)
    // Note: source_code_arc was created at the start of this function
    let static_ast = static_expression::convert_expressions(source_code_arc.clone(), ast);

    let function_registry = FunctionRegistry::new();
    let module_loader = ModuleLoader::default();
    let evaluation_result = match evaluate_with_registry(
        source_code_arc.clone(),
        static_ast,
        states_local_storage_key.clone(),
        virtual_fs,
        function_registry,
        module_loader,
    ) {
        Ok((root_object, construct_context, _registry, _module_loader, reference_connector, link_connector, pass_through_connector, link_variable_connector)) => {
            Some((root_object, construct_context, reference_connector, link_connector, pass_through_connector, link_variable_connector))
        }
        Err(error) => {
            println!("[Evaluation Error]");
            eprintln!("{error}");
            None
        }
    };

    if evaluation_result.is_some() {
        if let Err(error) = local_storage().insert(&old_code_local_storage_key, &source_code_for_storage) {
            eprintln!("Failed to store source code as old source code: {error:#?}");
        }

        if let Err(error) = local_storage().insert(
            &old_span_id_pairs_local_storage_key,
            &new_span_id_pairs.to_json_map().unwrap(),
        ) {
            eprintln!("Failed to store Span-PersistenceId pairs: {error:#}");
        }

        if let Some(states) =
            local_storage().get::<BTreeMap<String, serde_json::Value>>(&states_local_storage_key)
        {
            let mut states = states.expect("Failed to deseralize states");
            let persistent_ids = new_span_id_pairs
                .values()
                .map(|id| id.to_string())
                .collect::<HashSet<_>>();
            states.retain(|id, _| persistent_ids.contains(id));
            if let Err(error) = local_storage().insert(&states_local_storage_key, &states) {
                eprintln!("Failed to store states after removing old ones: {error:#?}");
            }
        }
    }

    evaluation_result
}

/// Run with function registry support for sharing functions across files.
/// Accepts an optional function registry and returns it along with the result.
/// This enables patterns like: run BUILD.bn, get its functions, pass to RUN.bn.
///
/// IMPORTANT: The returned `ReferenceConnector` and `LinkConnector` MUST be dropped
/// when the program is finished (e.g., when switching examples) to allow all actors
/// to be cleaned up. These connectors hold references to all top-level actors.
pub fn run_with_registry(
    filename: &str,
    source_code: &str,
    states_local_storage_key: impl Into<Cow<'static, str>>,
    old_code_local_storage_key: impl Into<Cow<'static, str>>,
    old_span_id_pairs_local_storage_key: impl Into<Cow<'static, str>>,
    virtual_fs: VirtualFilesystem,
    function_registry: Option<FunctionRegistry>,
) -> Option<(Arc<Object>, ConstructContext, FunctionRegistry, ModuleLoader, Arc<ReferenceConnector>, Arc<LinkConnector>, Arc<PassThroughConnector>, Arc<LinkVariableConnector>)> {
    println!("*** BOON INTERPRETER VERSION 2025-12-15-PARSER-DEBUG ***");
    let states_local_storage_key = states_local_storage_key.into();
    let old_code_local_storage_key = old_code_local_storage_key.into();
    let old_span_id_pairs_local_storage_key = old_span_id_pairs_local_storage_key.into();

    // Create SourceCode FIRST so all parsing borrows from this Arc'd String.
    // This is critical: the AST will contain &str slices that point into this allocation.
    // If we create SourceCode after parsing, the pointers won't match.
    let source_code_for_storage = source_code.to_string();
    let source_code_arc = SourceCode::new(source_code_for_storage.clone());
    let source_code = source_code_arc.as_str();

    let old_source_code = local_storage().get::<String>(&old_code_local_storage_key);
    let old_ast = if let Some(Ok(old_source_code)) = &old_source_code {
        parse_old(filename, old_source_code)
    } else {
        None
    };

    if LOG_SOURCE_AND_AST {
        println!("[Source Code ({filename})]");
        println!("{source_code}");
    }

    let (tokens, errors) = lexer().parse(source_code).into_output_errors();
    if !errors.is_empty() {
        println!("[Lex Errors]");
    }
    report_errors(errors, filename, source_code);
    let Some(mut tokens) = tokens else {
        return None;
    };

    tokens.retain(|spanned_token| !matches!(spanned_token.node, Token::Comment(_)));

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
        println!("[Parse Errors]");
    }
    report_errors(errors, filename, source_code);
    let Some(ast) = ast else {
        return None;
    };

    let ast = match resolve_references(ast) {
        Ok(ast) => ast,
        Err(errors) => {
            println!("[Reference Errors]");
            report_errors(errors, filename, source_code);
            return None;
        }
    };

    let (ast, new_span_id_pairs) =
        match resolve_persistence(ast, old_ast, &old_span_id_pairs_local_storage_key) {
            Ok(ast) => ast,
            Err(errors) => {
                println!("[Persistence Errors]");
                report_errors(errors, filename, source_code);
                return None;
            }
        };
    if LOG_SOURCE_AND_AST {
        println!("[Abstract Syntax Tree with Reference Data and Persistence]");
        println!("{ast:#?}");
    }

    // Convert to static expressions (owned, 'static, no lifetimes)
    // Note: source_code_arc was created at the start of this function
    let static_ast = static_expression::convert_expressions(source_code_arc.clone(), ast);

    let registry = function_registry.unwrap_or_default();
    let module_loader = ModuleLoader::default();
    let evaluation_result = match evaluate_with_registry(
        source_code_arc.clone(),
        static_ast,
        states_local_storage_key.clone(),
        virtual_fs,
        registry,
        module_loader,
    ) {
        Ok((root_object, construct_context, registry, module_loader, reference_connector, link_connector, pass_through_connector, link_variable_connector)) => {
            Some((root_object, construct_context, registry, module_loader, reference_connector, link_connector, pass_through_connector, link_variable_connector))
        }
        Err(error) => {
            println!("[Evaluation Error]");
            eprintln!("{error}");
            None
        }
    };

    if evaluation_result.is_some() {
        if let Err(error) = local_storage().insert(&old_code_local_storage_key, &source_code_for_storage) {
            eprintln!("Failed to store source code as old source code: {error:#?}");
        }

        if let Err(error) = local_storage().insert(
            &old_span_id_pairs_local_storage_key,
            &new_span_id_pairs.to_json_map().unwrap(),
        ) {
            eprintln!("Failed to store Span-PersistenceId pairs: {error:#}");
        }

        if let Some(states) =
            local_storage().get::<BTreeMap<String, serde_json::Value>>(&states_local_storage_key)
        {
            let mut states = states.expect("Failed to deseralize states");
            let persistent_ids = new_span_id_pairs
                .values()
                .map(|id| id.to_string())
                .collect::<HashSet<_>>();
            states.retain(|id, _| persistent_ids.contains(id));
            if let Err(error) = local_storage().insert(&states_local_storage_key, &states) {
                eprintln!("Failed to store states after removing old ones: {error:#?}");
            }
        }
    }

    evaluation_result
}

fn parse_old<'filename, 'old_code>(
    filename: &'filename str,
    source_code: &'old_code str,
) -> Option<Vec<Spanned<Expression<'old_code>>>> {
    let (tokens, errors) = lexer().parse(source_code).into_output_errors();
    if !errors.is_empty() {
        println!("[OLD Lex Errors]");
    }
    report_errors(errors, filename, source_code);
    let Some(mut tokens) = tokens else {
        return None;
    };

    tokens.retain(|spanned_token| !matches!(spanned_token.node, Token::Comment(_)));

    reset_expression_depth();
    let (ast, errors) = parser()
        .parse(Stream::from_iter(tokens).map(
            span_at(source_code.len()),
            |Spanned {
                 node,
                 span,
                 persistence: _,
             }| { (node, span) },
        ))
        .into_output_errors();
    if !errors.is_empty() {
        println!("[OLD Parse Errors]");
    }
    report_errors(errors, filename, source_code);
    let Some(ast) = ast else {
        return None;
    };

    let ast_with_reference_data = match resolve_references(ast) {
        Ok(ast_with_reference_data) => ast_with_reference_data,
        Err(errors) => {
            println!("[OLD Reference Errors]");
            report_errors(errors, filename, source_code);
            return None;
        }
    };
    Some(ast_with_reference_data)
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
