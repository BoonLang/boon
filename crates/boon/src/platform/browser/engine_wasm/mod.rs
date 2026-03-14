//! WASM compilation engine — compiles Boon source directly to WebAssembly bytecode.
//!
//! This module is gated behind the `engine-wasm` feature flag.
//! Pipeline: source → parse → IR lowering → WASM codegen → runtime instantiation → bridge UI
//!
//! For large programs (>4MB WASM binary), async compilation is used to bypass
//! Chrome's 8MB synchronous WebAssembly.Module.new() limit.

mod bridge;
mod codegen;
mod ir;
mod lower;
mod persistence;
mod runtime;

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Object, Reflect};
use wasm_bindgen::prelude::*;
use zoon::*;

use crate::parser::{
    SourceCode, Token, lexer, parser, reset_expression_depth, resolve_references, span_at,
    static_expression,
};

pub(crate) use persistence::clear_wasm_persisted_states;

thread_local! {
    static ACTIVE_WASM_INSTANCE: RefCell<Option<Rc<runtime::WasmInstance>>> = const { RefCell::new(None) };
}

/// External function definition for multi-file support.
use lower::ExternalFunction;

/// Threshold above which async compilation is used (4MB).
/// Chrome's sync limit is 8MB, so 4MB gives safety margin.
const ASYNC_COMPILE_THRESHOLD: usize = 4_000_000;

/// Run the WASM engine: compile source → generate WASM → instantiate → build UI.
/// Returns a Zoon element tree.
///
/// `external_functions` provides pre-parsed functions from other module files.
pub(super) fn run_wasm(
    source: &str,
    external_functions: Option<
        &[(
            String,
            Vec<String>,
            crate::parser::static_expression::Spanned<crate::parser::static_expression::Expression>,
            Option<String>,
        )],
    >,
    persistence_enabled: bool,
) -> RawElOrText {
    // 1. Parse and lower to IR.
    let program = match compile(source, external_functions) {
        Ok(p) => Rc::new(p),
        Err(msg) => return error_element(&msg),
    };

    // 2. Generate WASM binary.
    let wasm_output = codegen::emit_wasm(&program);

    // 3. Choose sync or async compilation based on binary size.
    if wasm_output.wasm_bytes.len() > ASYNC_COMPILE_THRESHOLD {
        run_wasm_async(program, wasm_output, source, persistence_enabled)
    } else {
        // Compile and instantiate synchronously for small modules.
        let wasm_buffer = js_sys::Uint8Array::from(&wasm_output.wasm_bytes[..]);
        let module = match js_sys::WebAssembly::Module::new(&wasm_buffer.into()) {
            Ok(module) => module,
            Err(error) => return error_element(&format!("WASM compile error: {:?}", error)),
        };
        let instance =
            match runtime::WasmInstance::new(&module, program.clone(), wasm_output.text_patterns) {
                Ok(instance) => instance,
                Err(error) => {
                    return error_element(&format!("WASM instantiation failed: {}", error));
                }
            };
        match finish_setup(instance, program, source, persistence_enabled) {
            Ok(ui) => ui,
            Err(msg) => error_element(&msg),
        }
    }
}

/// Async path: compile in background, show loading indicator, swap to real UI.
/// Used for large WASM binaries (>4MB) that exceed Chrome's sync limit.
fn run_wasm_async(
    program: Rc<ir::IrProgram>,
    wasm_output: codegen::WasmOutput,
    source: &str,
    persistence_enabled: bool,
) -> RawElOrText {
    let ui_storage: Rc<RefCell<Option<RawElOrText>>> = Rc::new(RefCell::new(None));
    let is_ready = Mutable::new(false);

    let ui_ref = ui_storage.clone();
    let ready = is_ready.clone();
    let source_owned = source.to_string();
    let persistence_enabled_owned = persistence_enabled;
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
        let parts = match runtime::WasmInstance::prepare(program.clone(), wasm_output.text_patterns)
        {
            Ok(p) => p,
            Err(msg) => {
                *ui_ref.borrow_mut() = Some(error_element(&msg));
                ready.set(true);
                return;
            }
        };

        // Instantiate asynchronously — no size limit on main thread.
        let instantiate_promise = js_sys::WebAssembly::instantiate_module(&module, &parts.imports);
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

        match finish_setup(instance, program, &source_owned, persistence_enabled_owned) {
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

    ACTIVE_WASM_INSTANCE.with(|slot| {
        let previous = slot.borrow_mut().replace(instance.clone());
        if let Some(old) = previous {
            old.shutdown();
        }
    });
    if let Some(window) = web_sys::window() {
        let api = Object::new();
        let instance_for_cell = instance.clone();
        let program_for_cell = program.clone();
        let program_for_find = program.clone();
        let get_cell = Closure::wrap(Box::new(move |name: String| -> JsValue {
            let result = Object::new();
            let found = program_for_cell
                .cells
                .iter()
                .enumerate()
                .find_map(|(idx, cell)| (cell.name == name).then_some(idx as u32));
            match found {
                Some(cell_id) => {
                    let _ = Reflect::set(&result, &"found".into(), &JsValue::TRUE);
                    let _ = Reflect::set(&result, &"id".into(), &JsValue::from(cell_id));
                    let _ = Reflect::set(
                        &result,
                        &"value".into(),
                        &JsValue::from(instance_for_cell.cell_store.get_cell_value(cell_id)),
                    );
                    let _ = Reflect::set(
                        &result,
                        &"text".into(),
                        &JsValue::from(instance_for_cell.cell_store.get_cell_text(cell_id)),
                    );
                }
                None => {
                    let _ = Reflect::set(&result, &"found".into(), &JsValue::FALSE);
                }
            }
            result.into()
        }) as Box<dyn Fn(String) -> JsValue>);
        let _ = Reflect::set(&api, &"getCell".into(), get_cell.as_ref());
        get_cell.forget();

        let find_cells = Closure::wrap(Box::new(move |pattern: String| -> JsValue {
            let result = js_sys::Array::new();
            for (idx, cell) in program_for_find.cells.iter().enumerate() {
                if cell.name.contains(&pattern) {
                    let entry = Object::new();
                    let _ = Reflect::set(&entry, &"id".into(), &JsValue::from(idx as u32));
                    let _ = Reflect::set(&entry, &"name".into(), &JsValue::from(cell.name.clone()));
                    result.push(&entry);
                }
            }
            result.into()
        }) as Box<dyn Fn(String) -> JsValue>);
        let _ = Reflect::set(&api, &"findCells".into(), find_cells.as_ref());
        find_cells.forget();

        let program_for_find_events = program.clone();
        let find_events = Closure::wrap(Box::new(move |pattern: String| -> JsValue {
            let result = js_sys::Array::new();
            for (idx, event) in program_for_find_events.events.iter().enumerate() {
                if event.name.contains(&pattern) {
                    let entry = Object::new();
                    let _ = Reflect::set(&entry, &"id".into(), &JsValue::from(idx as u32));
                    let _ =
                        Reflect::set(&entry, &"name".into(), &JsValue::from(event.name.clone()));
                    result.push(&entry);
                }
            }
            result.into()
        }) as Box<dyn Fn(String) -> JsValue>);
        let _ = Reflect::set(&api, &"findEvents".into(), find_events.as_ref());
        find_events.forget();

        let instance_for_item = instance.clone();
        let get_item_cell = Closure::wrap(Box::new(move |item_idx: u32, cell_id: u32| -> JsValue {
            let result = Object::new();
            if let Some(item_store) = instance_for_item.item_cell_store.as_ref() {
                let _ = Reflect::set(&result, &"found".into(), &JsValue::TRUE);
                let _ = Reflect::set(
                    &result,
                    &"value".into(),
                    &JsValue::from(item_store.get_value(item_idx, cell_id)),
                );
                let _ = Reflect::set(
                    &result,
                    &"text".into(),
                    &JsValue::from(item_store.get_text(item_idx, cell_id)),
                );
            } else {
                let _ = Reflect::set(&result, &"found".into(), &JsValue::FALSE);
            }
            result.into()
        }) as Box<dyn Fn(u32, u32) -> JsValue>);
        let _ = Reflect::set(&api, &"getItemCell".into(), get_item_cell.as_ref());
        get_item_cell.forget();

        let instance_for_dump = instance.clone();
        let program_for_dump = program.clone();
        let dump_item_cells = Closure::wrap(Box::new(move |item_idx: u32| -> JsValue {
            let result = js_sys::Array::new();
            let Some(item_store) = instance_for_dump.item_cell_store.as_ref() else {
                return result.into();
            };

            for (cell_id, value) in item_store.all_cell_values(item_idx) {
                let entry = Object::new();
                let _ = Reflect::set(&entry, &"id".into(), &JsValue::from(cell_id));
                if let Some(cell) = program_for_dump.cells.get(cell_id as usize) {
                    let _ = Reflect::set(&entry, &"name".into(), &JsValue::from(cell.name.clone()));
                }
                let _ = Reflect::set(&entry, &"value".into(), &JsValue::from(value));
                let text = item_store.get_text(item_idx, cell_id);
                if !text.is_empty() {
                    let _ = Reflect::set(&entry, &"text".into(), &JsValue::from(text));
                }
                result.push(&entry);
            }

            for (cell_id, text) in item_store.all_text_values(item_idx) {
                if text.is_empty() {
                    continue;
                }
                if item_store.get_value(item_idx, cell_id).is_nan() {
                    let entry = Object::new();
                    let _ = Reflect::set(&entry, &"id".into(), &JsValue::from(cell_id));
                    if let Some(cell) = program_for_dump.cells.get(cell_id as usize) {
                        let _ =
                            Reflect::set(&entry, &"name".into(), &JsValue::from(cell.name.clone()));
                    }
                    let _ = Reflect::set(&entry, &"text".into(), &JsValue::from(text));
                    result.push(&entry);
                }
            }

            result.into()
        }) as Box<dyn Fn(u32) -> JsValue>);
        let _ = Reflect::set(&api, &"dumpItemCells".into(), dump_item_cells.as_ref());
        dump_item_cells.forget();

        let program_for_name = program.clone();
        let cell_name = Closure::wrap(Box::new(move |cell_id: u32| -> JsValue {
            program_for_name
                .cells
                .get(cell_id as usize)
                .map(|cell| JsValue::from(cell.name.clone()))
                .unwrap_or(JsValue::NULL)
        }) as Box<dyn Fn(u32) -> JsValue>);
        let _ = Reflect::set(&api, &"cellName".into(), cell_name.as_ref());
        cell_name.forget();

        let program_for_event_name = program.clone();
        let event_name = Closure::wrap(Box::new(move |event_id: u32| -> JsValue {
            program_for_event_name
                .events
                .get(event_id as usize)
                .map(|event| JsValue::from(event.name.clone()))
                .unwrap_or(JsValue::NULL)
        }) as Box<dyn Fn(u32) -> JsValue>);
        let _ = Reflect::set(&api, &"eventName".into(), event_name.as_ref());
        event_name.forget();

        let instance_for_item_event = instance.clone();
        let fire_item_event =
            Closure::wrap(Box::new(move |item_idx: u32, event_id: u32| -> JsValue {
                let result = Object::new();
                match instance_for_item_event.call_on_item_event(item_idx, event_id, item_idx) {
                    Ok(()) => {
                        let _ = Reflect::set(&result, &"ok".into(), &JsValue::TRUE);
                    }
                    Err(error) => {
                        let _ = Reflect::set(&result, &"ok".into(), &JsValue::FALSE);
                        let _ = Reflect::set(&result, &"error".into(), &JsValue::from(error));
                    }
                }
                result.into()
            }) as Box<dyn Fn(u32, u32) -> JsValue>);
        let _ = Reflect::set(&api, &"fireItemEvent".into(), fire_item_event.as_ref());
        fire_item_event.forget();

        let _ = Reflect::set(window.as_ref(), &"__boonWasmDebug".into(), &api);
    }

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

    // 7. Register persistence save hook only when persistence is enabled.
    if restore_persistence {
        let save_inst = instance.clone();
        let save_key = storage_key.clone();
        instance.set_save_hook(Box::new(move || {
            persistence::save_and_store(&save_inst, &save_key);
        }));
    }

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

fn compile_ir_for_metrics(
    source: &str,
    external_functions: Option<&[ExternalFunction]>,
) -> Result<(ir::IrProgram, u128), String> {
    let started = std::time::Instant::now();
    let program = compile(source, external_functions)?;
    Ok((program, started.elapsed().as_millis()))
}

fn emit_wasm_output_for_metrics(program: &ir::IrProgram) -> codegen::WasmOutput {
    codegen::emit_wasm(program)
}

fn cells_bridge_metrics_for_metrics(
    program: &ir::IrProgram,
) -> Result<bridge::LegacyWasmBridgeMetrics, String> {
    bridge::cells_bridge_metrics_for_program(program)
}

fn legacy_wasm_module_metrics_for_metrics(
    source: &str,
    external_functions: Option<&[ExternalFunction]>,
) -> Result<super::LegacyWasmModuleMetrics, String> {
    let started = std::time::Instant::now();
    let (program, parse_lower_millis) = compile_ir_for_metrics(source, external_functions)?;
    let bridge_metrics = cells_bridge_metrics_for_metrics(&program)?;
    let emit_started = std::time::Instant::now();
    let wasm_output = emit_wasm_output_for_metrics(&program);
    let emit_millis = emit_started.elapsed().as_millis();
    let compile_millis = started.elapsed().as_millis();
    let first_render_total_proxy_millis =
        compile_millis.saturating_add(bridge_metrics.initial_materialize_millis);

    let mut import_count = 0;
    let mut export_count = 0;
    for payload in wasmparser::Parser::new(0).parse_all(&wasm_output.wasm_bytes) {
        match payload.map_err(|error| error.to_string())? {
            wasmparser::Payload::ImportSection(reader) => import_count += reader.count() as usize,
            wasmparser::Payload::ExportSection(reader) => export_count += reader.count() as usize,
            _ => {}
        }
    }

    Ok(super::LegacyWasmModuleMetrics {
        parse_lower_millis,
        emit_millis,
        bridge_initial_materialize_millis: bridge_metrics.initial_materialize_millis,
        bridge_edit_entry_proxy_millis: bridge_metrics.edit_entry_proxy_millis,
        bridge_dependent_recompute_proxy_millis: bridge_metrics.dependent_recompute_proxy_millis,
        first_render_total_proxy_millis,
        encoded_bytes: wasm_output.wasm_bytes.len(),
        import_count,
        export_count,
        compile_millis,
    })
}

pub(super) fn legacy_wasm_module_metrics_for_cells()
-> Result<super::LegacyWasmModuleMetrics, String> {
    let source = include_str!("../../../../../../playground/frontend/src/examples/cells/cells.bn");
    legacy_wasm_module_metrics_for_metrics(source, None)
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
