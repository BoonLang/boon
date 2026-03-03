//! WASM compilation engine — compiles Boon source directly to WebAssembly bytecode.
//!
//! This module is gated behind the `engine-wasm` feature flag.
//! Pipeline: source → parse → IR lowering → WASM codegen → runtime instantiation → bridge UI
//!
//! For large programs (>4MB WASM binary), async compilation is used to bypass
//! Chrome's 8MB synchronous WebAssembly.Module.new() limit.

pub mod bridge;
mod codegen;
pub mod ir;
mod lower;
mod persistence;
pub mod runtime;

use std::cell::RefCell;
use std::rc::Rc;

use zoon::*;

use crate::parser::{
    SourceCode, Token, lexer, parser, reset_expression_depth, resolve_references, span_at,
    static_expression,
};

pub use persistence::clear_wasm_persisted_states;

/// External function definition for multi-file support.
pub use lower::ExternalFunction;

/// Threshold above which async compilation is used (4MB).
/// Chrome's sync limit is 8MB, so 4MB gives safety margin.
const ASYNC_COMPILE_THRESHOLD: usize = 4_000_000;

/// Run the WASM engine: compile source → generate WASM → instantiate → build UI.
/// Returns a Zoon element tree.
///
/// `external_functions` provides pre-parsed functions from other module files.
pub fn run_wasm(source: &str, external_functions: Option<&[ExternalFunction]>) -> RawElOrText {
    // 1. Parse and lower to IR.
    let program = match compile(source, external_functions) {
        Ok(p) => Rc::new(p),
        Err(msg) => return error_element(&msg),
    };

    // 2. Generate WASM binary.
    let wasm_output = codegen::emit_wasm(&program);

    // 3. Choose sync or async compilation based on binary size.
    if wasm_output.wasm_bytes.len() > ASYNC_COMPILE_THRESHOLD {
        run_wasm_async(program, wasm_output, source)
    } else {
        match run_wasm_sync(program, wasm_output, source) {
            Ok(el) => el,
            Err(msg) => error_element(&msg),
        }
    }
}

/// Synchronous path: compile and instantiate immediately.
/// Used for small WASM binaries (<4MB).
fn run_wasm_sync(
    program: Rc<ir::IrProgram>,
    wasm_output: codegen::WasmOutput,
    source: &str,
) -> Result<RawElOrText, String> {
    // Compile synchronously.
    let wasm_buffer = js_sys::Uint8Array::from(&wasm_output.wasm_bytes[..]);
    let module = js_sys::WebAssembly::Module::new(&wasm_buffer.into())
        .map_err(|e| format!("WASM compile error: {:?}", e))?;

    // Instantiate synchronously (fine for small modules).
    let instance =
        runtime::WasmInstance::new(&module, program.clone(), wasm_output.text_patterns)
            .map_err(|e| format!("WASM instantiation failed: {}", e))?;

    finish_setup(instance, program, source, true)
}

/// Async path: compile in background, show loading indicator, swap to real UI.
/// Used for large WASM binaries (>4MB) that exceed Chrome's sync limit.
fn run_wasm_async(
    program: Rc<ir::IrProgram>,
    wasm_output: codegen::WasmOutput,
    source: &str,
) -> RawElOrText {
    let ui_storage: Rc<RefCell<Option<RawElOrText>>> = Rc::new(RefCell::new(None));
    let is_ready = Mutable::new(false);

    let ui_ref = ui_storage.clone();
    let ready = is_ready.clone();
    let source_owned = source.to_string();
    let bytes_len = wasm_output.wasm_bytes.len();

    Task::start(async move {
        // Compile asynchronously — no size limit.
        let buffer = js_sys::Uint8Array::from(&wasm_output.wasm_bytes[..]);
        let compile_promise = js_sys::WebAssembly::compile(&buffer.into());
        let module_result = JsFuture::from(compile_promise).await;
        let module: js_sys::WebAssembly::Module = match module_result {
            Ok(js) => js.unchecked_into(),
            Err(e) => {
                *ui_ref.borrow_mut() =
                    Some(error_element(&format!("WASM async compile error: {:?}", e)));
                ready.set(true);
                return;
            }
        };

        // Prepare imports and stores (sync, no WASM involved).
        let parts = match runtime::WasmInstance::prepare(
            program.clone(),
            wasm_output.text_patterns,
        ) {
            Ok(p) => p,
            Err(msg) => {
                *ui_ref.borrow_mut() = Some(error_element(&msg));
                ready.set(true);
                return;
            }
        };

        // Instantiate asynchronously — no size limit on main thread.
        let instantiate_promise =
            js_sys::WebAssembly::instantiate_module(&module, &parts.imports);
        let instance_result = JsFuture::from(instantiate_promise).await;
        let wasm_instance_js: js_sys::WebAssembly::Instance = match instance_result {
            Ok(js) => js.unchecked_into(),
            Err(e) => {
                *ui_ref.borrow_mut() = Some(error_element(&format!(
                    "WASM async instantiate error: {:?}",
                    e
                )));
                ready.set(true);
                return;
            }
        };

        // Finalize WasmInstance from the async-instantiated Instance.
        let instance = match runtime::WasmInstance::from_instance(parts, wasm_instance_js) {
            Ok(i) => i,
            Err(msg) => {
                *ui_ref.borrow_mut() = Some(error_element(&msg));
                ready.set(true);
                return;
            }
        };

        match finish_setup(instance, program, &source_owned, true) {
            Ok(ui) => *ui_ref.borrow_mut() = Some(ui),
            Err(msg) => *ui_ref.borrow_mut() = Some(error_element(&msg)),
        }
        ready.set(true);
    });

    // Return wrapper: loading message initially, swaps to real UI when ready.
    let ui_ref2 = ui_storage.clone();
    let size_mb = bytes_len as f64 / (1024.0 * 1024.0);
    El::new()
        .child_signal(is_ready.signal().map(move |ready| {
            if ready {
                ui_ref2.borrow_mut().take()
            } else {
                Some(
                    El::new()
                        .s(Font::new().color(color!("Gray")))
                        .child(format!("Compiling WASM ({:.1} MB)...", size_mb))
                        .into_raw_unchecked(),
                )
            }
        }))
        .unify()
}

/// Shared setup: router → init → persistence → timers → build UI.
/// The caller is responsible for creating the WasmInstance (sync or async).
fn finish_setup(
    instance: runtime::WasmInstance,
    program: Rc<ir::IrProgram>,
    source: &str,
    restore_persistence: bool,
) -> Result<RawElOrText, String> {
    // Wrap in Rc early (needed for router setup).
    let instance = Rc::new(instance);

    // 3. Set up router BEFORE init so WHEN/WHILE arms see route text.
    bridge::setup_router(&program, &instance);

    // 4. Call init() to set initial cell values.
    instance
        .call_init()
        .map_err(|e| format!("init() failed: {}", e))?;

    // 5. Load persisted snapshot (only on page refresh, not on re-run).
    let storage_key = persistence::storage_key(source);
    let snapshot = if restore_persistence {
        persistence::load_snapshot(&storage_key)
    } else {
        None
    };

    // 6. Phase 1 restore: global cells, texts, list structure, WASM memory.
    if let Some(snap) = snapshot {
        persistence::restore_phase1(&instance, &snap);
        instance.set_pending_snapshot(snap);
    }

    // 7. Register persistence save hook (fires after every event).
    let save_inst = instance.clone();
    let save_key = storage_key.clone();
    instance.set_save_hook(Box::new(move || {
        persistence::save_and_store(&save_inst, &save_key);
    }));

    // 8. Start timers.
    instance.start_timers(&program);

    // 9. Build Zoon element tree from IR + runtime.
    let ui = bridge::build_ui(&program, instance.clone());

    Ok(ui)
}

fn error_element(msg: &str) -> RawElOrText {
    El::new()
        .s(Font::new().color(color!("LightCoral")))
        .child(msg.to_string())
        .unify()
}

fn compile(
    source: &str,
    external_functions: Option<&[ExternalFunction]>,
) -> Result<ir::IrProgram, String> {
    let ast = parse_source(source)?;
    lower::lower(&ast, external_functions).map_err(|errors| format_errors(source, &errors))
}

fn format_errors(source: &str, errors: &[lower::CompileError]) -> String {
    use ariadne::{Config, Label, Report, ReportKind, Source};
    use std::io::Cursor;

    let filename = "source.bn";
    let mut out = String::new();
    let mut buf = Cursor::new(Vec::new());

    for err in errors {
        buf.set_position(0);
        buf.get_mut().clear();
        let span = err.span.start..err.span.end;
        let result = Report::build(ReportKind::Error, (filename, span.clone()))
            .with_config(Config::default().with_color(false))
            .with_message(&err.message)
            .with_label(Label::new((filename, span)).with_message(&err.message))
            .finish()
            .write((filename, Source::from(source)), &mut buf);

        if result.is_ok() {
            if let Ok(s) = String::from_utf8(buf.get_ref().clone()) {
                out.push_str(&s);
            }
        } else {
            // Fallback if ariadne fails.
            out.push_str(&format!(
                "[{}-{}] {}\n",
                err.span.start, err.span.end, err.message
            ));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Parser integration
// ---------------------------------------------------------------------------

fn parse_source(
    source_code: &str,
) -> Result<Vec<static_expression::Spanned<static_expression::Expression>>, String> {
    use chumsky::prelude::*;

    let source_code_arc = SourceCode::new(source_code.to_string());
    let source_for_parsing = source_code_arc.clone();
    let source_ref = source_for_parsing.as_str();

    // 1. Lex
    let (tokens, lex_errors) = lexer().parse(source_ref).into_output_errors();
    if !lex_errors.is_empty() {
        return Err(format!("Lex errors: {:?}", lex_errors));
    }
    let Some(mut tokens) = tokens else {
        return Err("Lexer produced no output".to_string());
    };

    // 2. Filter comments
    tokens.retain(|t| !matches!(t.node, Token::Comment(_)));

    // 3. Parse
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

    // 4. Resolve references
    let ast = resolve_references(ast).map_err(|e| format!("Reference errors: {:?}", e))?;

    // 5. Convert to static expressions
    let static_ast = static_expression::convert_expressions(source_code_arc, ast);
    Ok(static_ast)
}
