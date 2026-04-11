use std::borrow::Cow;
use std::fmt;
use std::io::{Cursor, Read};
use std::sync::Arc;

use ariadne::{Config, Label, Report, ReportKind, Source};
use chumsky::input::Stream;
use serde_json_any_key::MapIterToJson;
use zoon::{UnwrapThrowExt, WebStorage, eprintln, local_storage, println, serde_json};

/// Set to false to disable verbose source code and AST logging in console
const LOG_SOURCE_AND_AST: bool = false;

use crate::{
    engine::{
        ConstructContext, LinkConnector, Object, ReferenceConnector, ScopeDestroyGuard,
        VirtualFilesystem,
    },
    evaluator::{FunctionRegistry, ModuleLoader, evaluate_with_registry},
};
use boon::parser::{
    Expression, Input, ParseError, Parser, SourceCode, Spanned, Token, lexer, parser,
    reset_expression_depth, resolve_persistence, resolve_references, span_at, static_expression,
};

/// Run a Boon program and return the result.
///
/// IMPORTANT: The returned `ReferenceConnector`, `LinkConnector`, and `ScopeDestroyGuard`
/// MUST be dropped when the program is finished (e.g., when switching examples) to allow
/// all actors to be cleaned up. The connectors hold references to all top-level actors,
/// and the `ScopeDestroyGuard` recursively destroys all registry scopes and their actors.
pub fn run(
    filename: &str,
    source_code: &str,
    states_local_storage_key: impl Into<Cow<'static, str>>,
    old_code_local_storage_key: impl Into<Cow<'static, str>>,
    old_span_id_pairs_local_storage_key: impl Into<Cow<'static, str>>,
    virtual_fs: VirtualFilesystem,
) -> Option<(
    Arc<Object>,
    ConstructContext,
    Arc<ReferenceConnector>,
    Arc<LinkConnector>,
    ScopeDestroyGuard,
)> {
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
    if let Some(_tokens) = tokens.as_ref() {
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
    if let Some(_ast) = ast.as_ref() {
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

    let (ast, new_span_id_pairs, changed_variable_ids) =
        match resolve_persistence(ast, old_ast, &old_span_id_pairs_local_storage_key) {
            Ok(result) => result,
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

    // Clean stale persistence values for variables whose source code changed.
    // This prevents loading wrong values if a previous evaluation was interrupted
    // (e.g., by a panic) after old source code was stored but before value saves flushed.
    // Skip when persistence is disabled (empty key).
    if !changed_variable_ids.is_empty() && !states_local_storage_key.is_empty() {
        if let Some(Ok(mut states)) = local_storage()
            .get::<std::collections::BTreeMap<String, serde_json::Value>>(&states_local_storage_key)
        {
            let before = states.len();
            for id in &changed_variable_ids {
                states.remove(&id.to_string());
            }
            if states.len() != before {
                let _ = local_storage().insert(&states_local_storage_key, &states);
            }
        }
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
        Ok((
            root_object,
            construct_context,
            _registry,
            _module_loader,
            reference_connector,
            link_connector,
            root_scope_guard,
        )) => Some((
            root_object,
            construct_context,
            reference_connector,
            link_connector,
            root_scope_guard,
        )),
        Err(error) => {
            println!("[Evaluation Error]");
            eprintln!("{error}");
            None
        }
    };

    if evaluation_result.is_some() {
        if let Err(error) =
            local_storage().insert(&old_code_local_storage_key, &source_code_for_storage)
        {
            eprintln!("Failed to store source code as old source code: {error:#?}");
        }

        if let Err(error) = local_storage().insert(
            &old_span_id_pairs_local_storage_key,
            &new_span_id_pairs.to_json_map().unwrap(),
        ) {
            eprintln!("Failed to store Span-PersistenceId pairs: {error:#}");
        }

        // NOTE: We intentionally do NOT filter states based on span-ID pairs here.
        // Scoped IDs (created via persistence_id.in_scope()) are derived from base IDs
        // and won't match the span-ID pairs, but they are valid entries that should persist.
        // Filtering was removing these valid entries.
        // @TODO: Implement smarter cleanup that tracks parent-child ID relationships.
    }

    evaluation_result
}

/// Run with function registry support for sharing functions across files.
/// Accepts an optional function registry and returns it along with the result.
/// This enables patterns like: run BUILD.bn, get its functions, pass to RUN.bn.
///
/// IMPORTANT: The returned `ReferenceConnector`, `LinkConnector`, and `ScopeDestroyGuard`
/// MUST be dropped when the program is finished (e.g., when switching examples) to allow
/// all actors to be cleaned up. The connectors hold references to all top-level actors,
/// and the `ScopeDestroyGuard` recursively destroys all registry scopes and their actors.
pub fn run_with_registry(
    filename: &str,
    source_code: &str,
    states_local_storage_key: impl Into<Cow<'static, str>>,
    old_code_local_storage_key: impl Into<Cow<'static, str>>,
    old_span_id_pairs_local_storage_key: impl Into<Cow<'static, str>>,
    virtual_fs: VirtualFilesystem,
    function_registry: Option<FunctionRegistry>,
) -> Option<(
    Arc<Object>,
    ConstructContext,
    FunctionRegistry,
    ModuleLoader,
    Arc<ReferenceConnector>,
    Arc<LinkConnector>,
    ScopeDestroyGuard,
)> {
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

    let (ast, new_span_id_pairs, changed_variable_ids) =
        match resolve_persistence(ast, old_ast, &old_span_id_pairs_local_storage_key) {
            Ok(result) => result,
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

    // Clean stale persistence values for variables whose source code changed.
    // This prevents loading wrong values if a previous evaluation was interrupted
    // (e.g., by a panic) after old source code was stored but before value saves flushed.
    // Skip when persistence is disabled (empty key).
    if !changed_variable_ids.is_empty() && !states_local_storage_key.is_empty() {
        if let Some(Ok(mut states)) = local_storage()
            .get::<std::collections::BTreeMap<String, serde_json::Value>>(&states_local_storage_key)
        {
            let before = states.len();
            for id in &changed_variable_ids {
                states.remove(&id.to_string());
            }
            if states.len() != before {
                let _ = local_storage().insert(&states_local_storage_key, &states);
            }
        }
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
        Ok((
            root_object,
            construct_context,
            registry,
            module_loader,
            reference_connector,
            link_connector,
            root_scope_guard,
        )) => Some((
            root_object,
            construct_context,
            registry,
            module_loader,
            reference_connector,
            link_connector,
            root_scope_guard,
        )),
        Err(error) => {
            println!("[Evaluation Error]");
            eprintln!("{error}");
            None
        }
    };

    if evaluation_result.is_some() {
        if let Err(error) =
            local_storage().insert(&old_code_local_storage_key, &source_code_for_storage)
        {
            eprintln!("Failed to store source code as old source code: {error:#?}");
        }

        if let Err(error) = local_storage().insert(
            &old_span_id_pairs_local_storage_key,
            &new_span_id_pairs.to_json_map().unwrap(),
        ) {
            eprintln!("Failed to store Span-PersistenceId pairs: {error:#}");
        }

        // NOTE: We intentionally do NOT filter states based on span-ID pairs here.
        // Scoped IDs (created via persistence_id.in_scope()) are derived from base IDs
        // and won't match the span-ID pairs, but they are valid entries that should persist.
        // Filtering was removing these valid entries.
        // @TODO: Implement smarter cleanup that tracks parent-child ID relationships.
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

#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use super::run_with_registry;
    use crate::engine::{Value, VirtualFilesystem};
    use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
    use zoon::Timer;
    use zoon::futures_util::{FutureExt, StreamExt, future::select, pin_mut};

    wasm_bindgen_test_configure!(run_in_browser);

    async fn next_value_with_timeout<S>(stream: &mut S, timeout_ms: u32, context: &str) -> Value
    where
        S: zoon::futures_util::stream::Stream<Item = Value> + Unpin,
    {
        let next_value = stream.next().fuse();
        let timeout = Timer::sleep(timeout_ms).fuse();
        pin_mut!(next_value);
        pin_mut!(timeout);

        match select(next_value, timeout).await {
            zoon::futures_util::future::Either::Left((Some(value), _)) => value,
            zoon::futures_util::future::Either::Left((None, _)) => {
                panic!("{context}: stream ended before emitting a value")
            }
            zoon::futures_util::future::Either::Right((_, _)) => {
                panic!("{context}: timed out waiting for the next value")
            }
        }
    }

    async fn assert_no_value_before_timeout<S>(stream: &mut S, timeout_ms: u32, context: &str)
    where
        S: zoon::futures_util::stream::Stream<Item = Value> + Unpin,
    {
        let next_value = stream.next().fuse();
        let timeout = Timer::sleep(timeout_ms).fuse();
        pin_mut!(next_value);
        pin_mut!(timeout);

        match select(next_value, timeout).await {
            zoon::futures_util::future::Either::Left((Some(_), _)) => {
                panic!("{context}: unexpected early value")
            }
            zoon::futures_util::future::Either::Left((None, _)) => {
                panic!("{context}: stream ended before timeout")
            }
            zoon::futures_util::future::Either::Right((_, _)) => {}
        }
    }

    #[wasm_bindgen_test(async)]
    async fn parsed_interval_then_sum_variable_stream_updates_across_multiple_ticks() {
        let source = r#"
value:
    Duration[milliseconds: 50]
    |> Timer/interval()
    |> THEN { 1 }
    |> Math/sum()
"#;

        let (
            object,
            _construct_context,
            _registry,
            _module_loader,
            _reference_connector,
            _link_connector,
            _root_scope_guard,
        ) = run_with_registry(
            "parsed_interval_then_sum_variable_stream_test.bn",
            source,
            "",
            "",
            "",
            VirtualFilesystem::new(),
            None,
        )
        .expect("parsed interval-then-sum variable source should run");

        let value_actor = object.expect_variable("value").value_actor();
        let stream = value_actor.current_or_future_stream();
        pin_mut!(stream);

        let first = next_value_with_timeout(
            &mut stream,
            2_000,
            "parsed interval-then-sum variable first tick",
        )
        .await;
        let second = next_value_with_timeout(
            &mut stream,
            2_000,
            "parsed interval-then-sum variable second tick",
        )
        .await;

        let Value::Number(first, _) = first else {
            panic!("first parsed interval-then-sum variable tick should be a number");
        };
        assert_eq!(first.number(), 1.0);

        let Value::Number(second, _) = second else {
            panic!("second parsed interval-then-sum variable tick should be a number");
        };
        assert_eq!(second.number(), 2.0);
    }

    #[wasm_bindgen_test(async)]
    async fn parsed_interval_hold_variable_stream_updates_across_multiple_ticks() {
        let source = r#"
tick: Duration[milliseconds: 50] |> Timer/interval()

counter:
    0
    |> HOLD counter {
        tick |> THEN { counter + 1 }
    }
    |> Stream/skip(count: 1)
"#;

        let (
            object,
            _construct_context,
            _registry,
            _module_loader,
            _reference_connector,
            _link_connector,
            _root_scope_guard,
        ) = run_with_registry(
            "parsed_interval_hold_variable_stream_test.bn",
            source,
            "",
            "",
            "",
            VirtualFilesystem::new(),
            None,
        )
        .expect("parsed interval_hold variable source should run");

        let value_actor = object.expect_variable("counter").value_actor();
        let stream = value_actor.current_or_future_stream();
        pin_mut!(stream);

        let first =
            next_value_with_timeout(&mut stream, 2_000, "parsed interval_hold first tick").await;
        let second =
            next_value_with_timeout(&mut stream, 2_000, "parsed interval_hold second tick").await;

        let Value::Number(first, _) = first else {
            panic!("first parsed interval_hold tick should be a number");
        };
        assert_eq!(first.number(), 1.0);

        let Value::Number(second, _) = second else {
            panic!("second parsed interval_hold tick should be a number");
        };
        assert_eq!(second.number(), 2.0);
    }

    #[wasm_bindgen_test(async)]
    async fn parsed_timer_elapsed_variable_caps_after_multiple_ticks() {
        let source = r#"
tick: Duration[milliseconds: 50] |> Timer/interval()

raw_elapsed:
    0
    |> HOLD state {
        tick |> THEN { state + 0.1 }
    }

elapsed: raw_elapsed |> Math/min(b: 0.2)
"#;

        let (
            object,
            _construct_context,
            _registry,
            _module_loader,
            _reference_connector,
            _link_connector,
            _root_scope_guard,
        ) = run_with_registry(
            "parsed_timer_elapsed_variable_stream_test.bn",
            source,
            "",
            "",
            "",
            VirtualFilesystem::new(),
            None,
        )
        .expect("parsed timer elapsed variable source should run");

        let value_actor = object.expect_variable("elapsed").value_actor();
        let stream = value_actor.current_or_future_stream();
        pin_mut!(stream);

        let initial =
            next_value_with_timeout(&mut stream, 500, "parsed timer elapsed initial value").await;
        let first =
            next_value_with_timeout(&mut stream, 2_000, "parsed timer elapsed first tick").await;
        let second =
            next_value_with_timeout(&mut stream, 2_000, "parsed timer elapsed second tick").await;

        let Value::Number(initial, _) = initial else {
            panic!("initial parsed timer elapsed value should be a number");
        };
        assert!((initial.number() - 0.0).abs() < f64::EPSILON);

        let Value::Number(first, _) = first else {
            panic!("first parsed timer elapsed tick should be a number");
        };
        assert!((first.number() - 0.1).abs() < 0.000_001);

        let Value::Number(second, _) = second else {
            panic!("second parsed timer elapsed tick should be a number");
        };
        assert!((second.number() - 0.2).abs() < 0.000_001);
    }

    #[wasm_bindgen_test(async)]
    async fn parsed_hold_latest_reset_discards_stale_pre_reset_tick_results() {
        let source = r#"
tick: Duration[milliseconds: 40] |> Timer/interval()
reset: Duration[milliseconds: 180] |> Timer/interval()

counter:
    0
    |> HOLD state {
        LATEST {
            tick |> THEN { state + 1 }
            reset |> THEN { 0 }
        }
    }
    |> Stream/skip(count: 1)
"#;

        let (
            object,
            _construct_context,
            _registry,
            _module_loader,
            _reference_connector,
            _link_connector,
            _root_scope_guard,
        ) = run_with_registry(
            "parsed_hold_latest_reset_stream_test.bn",
            source,
            "",
            "",
            "",
            VirtualFilesystem::new(),
            None,
        )
        .expect("parsed HOLD LATEST reset source should run");

        let value_actor = object.expect_variable("counter").value_actor();
        let stream = value_actor.current_or_future_stream();
        pin_mut!(stream);

        let first = next_value_with_timeout(&mut stream, 2_000, "first pre-reset tick").await;
        let second = next_value_with_timeout(&mut stream, 2_000, "second pre-reset tick").await;
        let third = next_value_with_timeout(&mut stream, 2_000, "third pre-reset tick").await;
        let fourth = next_value_with_timeout(&mut stream, 2_000, "fourth pre-reset tick").await;
        let reset_value = next_value_with_timeout(&mut stream, 2_000, "reset value").await;
        let first_after_reset =
            next_value_with_timeout(&mut stream, 2_000, "first post-reset tick").await;

        let Value::Number(first, _) = first else {
            panic!("first pre-reset tick should be a number");
        };
        assert_eq!(first.number(), 1.0);

        let Value::Number(second, _) = second else {
            panic!("second pre-reset tick should be a number");
        };
        assert_eq!(second.number(), 2.0);

        let Value::Number(third, _) = third else {
            panic!("third pre-reset tick should be a number");
        };
        assert_eq!(third.number(), 3.0);

        let Value::Number(fourth, _) = fourth else {
            panic!("fourth pre-reset tick should be a number");
        };
        assert_eq!(fourth.number(), 4.0);

        let Value::Number(reset_value, _) = reset_value else {
            panic!("reset value should be a number");
        };
        assert_eq!(reset_value.number(), 0.0);

        let Value::Number(first_after_reset, _) = first_after_reset else {
            panic!("first post-reset tick should be a number");
        };
        assert_eq!(first_after_reset.number(), 1.0);
    }

    #[wasm_bindgen_test(async)]
    async fn parsed_then_variable_waits_for_trigger_before_emitting_snapshot() {
        let source = r#"
tick: Duration[milliseconds: 40] |> Timer/interval()
trigger: Duration[milliseconds: 180] |> Timer/interval()

input:
    0
    |> HOLD sum {
        tick |> THEN { sum + 1 }
    }

captured: trigger |> THEN { input }
"#;

        let (
            object,
            _construct_context,
            _registry,
            _module_loader,
            _reference_connector,
            _link_connector,
            _root_scope_guard,
        ) = run_with_registry(
            "parsed_then_variable_trigger_test.bn",
            source,
            "",
            "",
            "",
            VirtualFilesystem::new(),
            None,
        )
        .expect("parsed then variable source should run");

        let value_actor = object.expect_variable("captured").value_actor();
        let stream = value_actor.current_or_future_stream();
        pin_mut!(stream);

        assert_no_value_before_timeout(
            &mut stream,
            100,
            "parsed THEN result should stay pending before the trigger fires",
        )
        .await;

        let first = next_value_with_timeout(&mut stream, 2_000, "parsed THEN first trigger").await;
        let second =
            next_value_with_timeout(&mut stream, 2_000, "parsed THEN second trigger").await;

        let Value::Number(first, _) = first else {
            panic!("first parsed THEN trigger should produce a number");
        };
        assert!(
            first.number() >= 2.0,
            "first parsed THEN snapshot should reflect accumulated input ticks"
        );

        let Value::Number(second, _) = second else {
            panic!("second parsed THEN trigger should produce a number");
        };
        assert!(
            second.number() > first.number(),
            "later parsed THEN trigger should capture a newer snapshot"
        );
    }

    #[wasm_bindgen_test(async)]
    async fn parsed_when_variable_ignores_input_ticks_until_branch_switch() {
        let source = r#"
tick_a: Duration[milliseconds: 40] |> Timer/interval()
tick_b: Duration[milliseconds: 60] |> Timer/interval()
switch: Duration[milliseconds: 180] |> Timer/interval() |> THEN { Subtraction }

input_a:
    0
    |> HOLD sum {
        tick_a |> THEN { sum + 1 }
    }

input_b:
    0
    |> HOLD sum {
        tick_b |> THEN { sum + 10 }
    }

operation: LATEST {
    Addition
    switch
}

current_result: operation |> WHEN {
    Addition => input_a + input_b
    Subtraction => input_a - input_b
}
"#;

        let (
            object,
            _construct_context,
            _registry,
            _module_loader,
            _reference_connector,
            _link_connector,
            _root_scope_guard,
        ) = run_with_registry(
            "parsed_when_variable_switch_test.bn",
            source,
            "",
            "",
            "",
            VirtualFilesystem::new(),
            None,
        )
        .expect("parsed when variable source should run");

        let value_actor = object.expect_variable("current_result").value_actor();
        let stream = value_actor.current_or_future_stream();
        pin_mut!(stream);

        let initial = next_value_with_timeout(&mut stream, 500, "parsed WHEN initial value").await;
        let Value::Number(initial, _) = initial else {
            panic!("initial parsed WHEN value should be a number");
        };
        assert!((initial.number() - 0.0).abs() < f64::EPSILON);

        assert_no_value_before_timeout(
            &mut stream,
            120,
            "parsed WHEN result should ignore input ticks while branch stays the same",
        )
        .await;

        let switched =
            next_value_with_timeout(&mut stream, 2_000, "parsed WHEN switched branch").await;
        let Value::Number(switched, _) = switched else {
            panic!("switched parsed WHEN value should be a number");
        };
        assert!(
            switched.number() < 0.0,
            "parsed WHEN should recompute once the operation branch changes"
        );
    }

    #[wasm_bindgen_test(async)]
    async fn parsed_while_variable_keeps_updating_with_active_branch_ticks() {
        let source = r#"
tick_a: Duration[milliseconds: 40] |> Timer/interval()
tick_b: Duration[milliseconds: 60] |> Timer/interval()
switch: Duration[milliseconds: 180] |> Timer/interval() |> THEN { Subtraction }

input_a:
    0
    |> HOLD sum {
        tick_a |> THEN { sum + 1 }
    }

input_b:
    0
    |> HOLD sum {
        tick_b |> THEN { sum + 10 }
    }

operation: LATEST {
    Addition
    switch
}

current_result: operation |> WHILE {
    Addition => input_a + input_b
    Subtraction => input_a - input_b
}
"#;

        let (
            object,
            _construct_context,
            _registry,
            _module_loader,
            _reference_connector,
            _link_connector,
            _root_scope_guard,
        ) = run_with_registry(
            "parsed_while_variable_switch_test.bn",
            source,
            "",
            "",
            "",
            VirtualFilesystem::new(),
            None,
        )
        .expect("parsed while variable source should run");

        let value_actor = object.expect_variable("current_result").value_actor();
        let stream = value_actor.current_or_future_stream();
        pin_mut!(stream);

        let initial = next_value_with_timeout(&mut stream, 500, "parsed WHILE initial value").await;
        let Value::Number(initial, _) = initial else {
            panic!("initial parsed WHILE value should be a number");
        };
        assert!((initial.number() - 0.0).abs() < f64::EPSILON);

        let while_update =
            next_value_with_timeout(&mut stream, 120, "parsed WHILE update before branch switch")
                .await;
        let Value::Number(while_update, _) = while_update else {
            panic!("parsed WHILE update before branch switch should be a number");
        };
        assert!(
            while_update.number() > 0.0,
            "parsed WHILE should keep updating while the initial branch stays active"
        );

        let switched =
            next_value_with_timeout(&mut stream, 2_000, "parsed WHILE switched branch").await;
        let Value::Number(switched, _) = switched else {
            panic!("parsed WHILE switched value should be a number");
        };
        assert!(
            switched.number() < while_update.number(),
            "parsed WHILE should switch to the new branch after the operation changes"
        );
    }
}
