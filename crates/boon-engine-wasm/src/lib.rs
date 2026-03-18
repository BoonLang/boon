//! Wasm backend.
#![allow(dead_code)]

pub mod abi;
mod codegen;
mod debug;
mod exec_ir;
mod lower;
mod runtime;
mod semantic_ir;

use boon_renderer_zoon::{FakeRenderState, missing_document_root};
use boon_scene::{
    RenderDiffBatch, RenderRoot, UiEvent, UiEventBatch, UiEventKind, UiFactBatch, UiNode,
    UiNodeKind,
};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsValue;
#[cfg(target_arch = "wasm32")]
use zoon::js_sys::Reflect;
use zoon::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendBatchMetrics {
    pub encoded_bytes: usize,
    pub op_count: usize,
    pub ui_node_count: usize,
    pub double_click_ports: usize,
    pub input_ports: usize,
    pub key_down_ports: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmPipelineMetrics {
    pub lower_millis: u128,
    pub exec_build_millis: u128,
    pub lower_exec_millis: u128,
    pub init_runtime_millis: u128,
    pub init_decode_apply_millis: u128,
    pub init_millis: u128,
    pub first_render_total_millis: u128,
    pub edit_entry_millis: u128,
    pub input_update_millis: u128,
    pub commit_runtime_millis: u128,
    pub commit_decode_apply_millis: u128,
    pub a1_commit_millis: u128,
    pub dependent_recompute_runtime_millis: u128,
    pub dependent_recompute_decode_apply_millis: u128,
    pub a2_recompute_millis: u128,
    pub init_batch: BackendBatchMetrics,
    pub a1_commit_batch: BackendBatchMetrics,
    pub a2_recompute_batch: BackendBatchMetrics,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CellsBackendMetricsReport {
    pub wasm: WasmPipelineMetrics,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CellsBackendComparison {
    pub module_under_size_budget: bool,
    pub incremental_commit_under_size_budget: bool,
    pub incremental_commit_under_op_budget: bool,
    pub first_render_under_time_budget: bool,
    pub edit_path_under_time_budget: bool,
    pub dependent_recompute_under_time_budget: bool,
}

impl CellsBackendComparison {
    #[must_use]
    pub fn from_report(report: &CellsBackendMetricsReport) -> Self {
        Self {
            module_under_size_budget: report.wasm.init_batch.encoded_bytes <= 1_000_000,
            incremental_commit_under_size_budget: report.wasm.a1_commit_batch.encoded_bytes
                <= 2_000,
            incremental_commit_under_op_budget: report.wasm.a1_commit_batch.op_count <= 16,
            first_render_under_time_budget: report.wasm.first_render_total_millis <= 300,
            edit_path_under_time_budget: report.wasm.a1_commit_millis <= 300,
            dependent_recompute_under_time_budget: report.wasm.a2_recompute_millis <= 300,
        }
    }

    #[must_use]
    pub fn all_pass(&self) -> bool {
        self.module_under_size_budget
            && self.incremental_commit_under_size_budget
            && self.incremental_commit_under_op_budget
            && self.first_render_under_time_budget
            && self.edit_path_under_time_budget
            && self.dependent_recompute_under_time_budget
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmLoweringCheckResult {
    pub example_name: String,
    pub passed: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmLoweringReport {
    pub examples: Vec<WasmLoweringCheckResult>,
}

impl WasmLoweringReport {
    #[must_use]
    pub fn all_pass(&self) -> bool {
        self.examples.iter().all(|example| example.passed)
    }
}

type ExternalFunction = (
    String,
    Vec<String>,
    boon::parser::static_expression::Spanned<boon::parser::static_expression::Expression>,
    Option<String>,
);

const WASM_PERSISTENCE_KEY_PREFIX: &str = "boon-wasm-state:";

#[derive(Debug, Clone, Serialize, Deserialize)]
enum PersistedWasmBatch {
    Events(UiEventBatch),
    Facts(UiFactBatch),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedWasmState {
    history: Vec<PersistedWasmBatch>,
}

fn ui_text_content(node: &UiNode) -> Option<&str> {
    match &node.kind {
        UiNodeKind::Text { text } => Some(text.as_str()),
        UiNodeKind::Element { text, .. } => text.as_deref(),
    }
}

fn ui_node_count(node: &UiNode) -> usize {
    1 + node.children.iter().map(ui_node_count).sum::<usize>()
}

#[cfg(target_arch = "wasm32")]
fn wasm_browser_log(message: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Ok(console) = Reflect::get(window.as_ref(), &JsValue::from_str("console")) else {
        return;
    };
    let Ok(log) = Reflect::get(&console, &JsValue::from_str("log")) else {
        return;
    };
    let Some(log) = log.dyn_ref::<js_sys::Function>() else {
        return;
    };
    let _ = log.call1(&console, &JsValue::from_str(message));
}

#[cfg(not(target_arch = "wasm32"))]
fn wasm_browser_log(_message: &str) {}

#[cfg(target_arch = "wasm32")]
fn wasm_set_debug_attr(name: &str, value: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(document) = window.document() else {
        return;
    };
    let Some(root) = document.document_element() else {
        return;
    };
    let _ = root.set_attribute(name, value);
}

#[cfg(not(target_arch = "wasm32"))]
fn wasm_set_debug_attr(_name: &str, _value: &str) {}

fn attached_port_count(batch: &RenderDiffBatch, expected_kind: UiEventKind) -> usize {
    batch
        .ops
        .iter()
        .filter(|op| {
            matches!(
                op,
                boon_scene::RenderOp::AttachEventPort { kind, .. } if *kind == expected_kind
            )
        })
        .count()
}

fn batch_metrics(batch: &RenderDiffBatch) -> BackendBatchMetrics {
    BackendBatchMetrics {
        encoded_bytes: abi::encode_render_diff_batch(batch).len(),
        op_count: batch.ops.len(),
        ui_node_count: batch
            .ops
            .iter()
            .find_map(|op| match op {
                boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) => {
                    Some(ui_node_count(root))
                }
                _ => None,
            })
            .unwrap_or_default(),
        double_click_ports: attached_port_count(batch, UiEventKind::DoubleClick),
        input_ports: attached_port_count(batch, UiEventKind::Input),
        key_down_ports: attached_port_count(batch, UiEventKind::KeyDown),
    }
}

fn apply_batch_and_root(
    state: &mut FakeRenderState,
    batch: &RenderDiffBatch,
) -> Result<UiNode, String> {
    state
        .apply_batch(batch)
        .map_err(|error| format!("batch should apply: {error:?}"))?;
    let Some(RenderRoot::UiTree(root)) = state.root() else {
        return Err("expected ui root after batch".to_string());
    };
    Ok(root.clone())
}

fn find_first_tag<'a>(node: &'a UiNode, expected_tag: &str) -> Option<&'a UiNode> {
    if matches!(&node.kind, UiNodeKind::Element { tag, .. } if tag == expected_tag) {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| find_first_tag(child, expected_tag))
}

fn visit_ui_nodes<'a>(node: &'a UiNode, out: &mut Vec<&'a UiNode>) {
    out.push(node);
    for child in &node.children {
        visit_ui_nodes(child, out);
    }
}

fn find_nth_port_in_state(
    root: &UiNode,
    state: &FakeRenderState,
    expected_kind: UiEventKind,
    ordinal: usize,
) -> Option<boon_scene::EventPortId> {
    let mut nodes = Vec::new();
    visit_ui_nodes(root, &mut nodes);
    let mut seen = 0;
    for node in nodes {
        for (port, kind) in state.event_ports_for(node.id) {
            if kind == expected_kind {
                if seen == ordinal {
                    return Some(port);
                }
                seen += 1;
            }
        }
    }
    None
}

fn dispatch_text_input_commit_with_state(
    runtime: &mut runtime::WasmRuntime,
    state: &mut FakeRenderState,
    ordinal: usize,
    value: &str,
) -> Result<(u128, u128, u128, u128, RenderDiffBatch), String> {
    let Some(RenderRoot::UiTree(root)) = state.root() else {
        return Err("expected cells root before editing".to_string());
    };
    let double_click_port = find_nth_port_in_state(root, state, UiEventKind::DoubleClick, ordinal)
        .ok_or_else(|| "target cell should expose DoubleClick".to_string())?;

    let edit_started = Instant::now();
    let edit_descriptor = runtime
        .dispatch_events(&abi::encode_ui_event_batch(&UiEventBatch {
            events: vec![UiEvent {
                target: double_click_port,
                kind: UiEventKind::DoubleClick,
                payload: None,
            }],
        }))
        .map_err(|error| error.to_string())?;
    let edit_batch = runtime
        .decode_commands(edit_descriptor)
        .map_err(|error| error.to_string())?;
    let edit_root = apply_batch_and_root(state, &edit_batch)?;
    let edit_entry_millis = edit_started.elapsed().as_millis();
    let input = find_first_tag(&edit_root, "input")
        .ok_or_else(|| "edit mode should render input".to_string())?;
    let input_port = state
        .event_ports_for(input.id)
        .into_iter()
        .find_map(|(port, kind)| (kind == UiEventKind::Input).then_some(port))
        .ok_or_else(|| "edit input should expose Input".to_string())?;
    let key_down_port = state
        .event_ports_for(input.id)
        .into_iter()
        .find_map(|(port, kind)| (kind == UiEventKind::KeyDown).then_some(port))
        .ok_or_else(|| "edit input should expose KeyDown".to_string())?;

    let input_started = Instant::now();
    let input_descriptor = runtime
        .dispatch_events(&abi::encode_ui_event_batch(&UiEventBatch {
            events: vec![UiEvent {
                target: input_port,
                kind: UiEventKind::Input,
                payload: Some(value.to_string()),
            }],
        }))
        .map_err(|error| error.to_string())?;
    if input_descriptor != 0 {
        return Err("input update unexpectedly produced a batch".to_string());
    }
    let input_update_millis = input_started.elapsed().as_millis();

    let commit_runtime_started = Instant::now();
    let commit_descriptor = runtime
        .dispatch_events(&abi::encode_ui_event_batch(&UiEventBatch {
            events: vec![UiEvent {
                target: key_down_port,
                kind: UiEventKind::KeyDown,
                payload: Some("Enter".to_string()),
            }],
        }))
        .map_err(|error| error.to_string())?;
    let commit_runtime_millis = commit_runtime_started.elapsed().as_millis();
    let commit_apply_started = Instant::now();
    let commit_batch = runtime
        .decode_commands(commit_descriptor)
        .map_err(|error| error.to_string())?;
    let _ = apply_batch_and_root(state, &commit_batch)?;
    let commit_decode_apply_millis = commit_apply_started.elapsed().as_millis();

    Ok((
        edit_entry_millis,
        input_update_millis,
        commit_runtime_millis,
        commit_decode_apply_millis,
        commit_batch,
    ))
}

fn edit_nth_cells_grid_cell_and_commit_batch_with_state(
    runtime: &mut runtime::WasmRuntime,
    state: &mut FakeRenderState,
    ordinal: usize,
    value: &str,
) -> Result<RenderDiffBatch, String> {
    let init_descriptor = runtime.take_commands();
    if init_descriptor != 0 {
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .map_err(|error| error.to_string())?;
        let _ = apply_batch_and_root(state, &init_batch)?;
    }
    let Some(RenderRoot::UiTree(root)) = state.root() else {
        return Err("expected cells root before editing".to_string());
    };
    let double_click_port = find_nth_port_in_state(root, state, UiEventKind::DoubleClick, ordinal)
        .ok_or_else(|| "target cell should expose DoubleClick".to_string())?;

    let edit_descriptor = runtime
        .dispatch_events(&abi::encode_ui_event_batch(&UiEventBatch {
            events: vec![UiEvent {
                target: double_click_port,
                kind: UiEventKind::DoubleClick,
                payload: None,
            }],
        }))
        .map_err(|error| error.to_string())?;
    let edit_batch = runtime
        .decode_commands(edit_descriptor)
        .map_err(|error| error.to_string())?;
    let root = apply_batch_and_root(state, &edit_batch)?;
    let input = find_first_tag(&root, "input")
        .ok_or_else(|| "edit mode should render input".to_string())?;
    let input_port = state
        .event_ports_for(input.id)
        .into_iter()
        .find_map(|(port, kind)| (kind == UiEventKind::Input).then_some(port))
        .ok_or_else(|| "edit input should expose Input".to_string())?;
    let key_down_port = state
        .event_ports_for(input.id)
        .into_iter()
        .find_map(|(port, kind)| (kind == UiEventKind::KeyDown).then_some(port))
        .ok_or_else(|| "edit input should expose KeyDown".to_string())?;

    let input_descriptor = runtime
        .dispatch_events(&abi::encode_ui_event_batch(&UiEventBatch {
            events: vec![UiEvent {
                target: input_port,
                kind: UiEventKind::Input,
                payload: Some(value.to_string()),
            }],
        }))
        .map_err(|error| error.to_string())?;
    if input_descriptor != 0 {
        return Err("input update unexpectedly produced a batch".to_string());
    }

    let commit_descriptor = runtime
        .dispatch_events(&abi::encode_ui_event_batch(&UiEventBatch {
            events: vec![UiEvent {
                target: key_down_port,
                kind: UiEventKind::KeyDown,
                payload: Some("Enter".to_string()),
            }],
        }))
        .map_err(|error| error.to_string())?;
    let commit_batch = runtime
        .decode_commands(commit_descriptor)
        .map_err(|error| error.to_string())?;
    let _ = apply_batch_and_root(state, &commit_batch)?;
    Ok(commit_batch)
}

pub fn clear_wasm_persisted_states() {
    #[cfg(target_arch = "wasm32")]
    {
        let Some(storage) = browser_local_storage() else {
            return;
        };
        let Ok(length) = storage.length() else {
            return;
        };
        let mut keys_to_remove = Vec::new();
        for index in 0..length {
            let Ok(Some(key)) = storage.key(index) else {
                continue;
            };
            if key.starts_with(WASM_PERSISTENCE_KEY_PREFIX) {
                keys_to_remove.push(key);
            }
        }
        for key in keys_to_remove {
            let _ = storage.remove_item(&key);
        }
    }
}

pub fn try_lower_for_verification(source: &str) -> Result<(), String> {
    lower::try_lower_to_semantic(source, None, false).map(|_| ())
}

pub fn wasm_pipeline_metrics_for_cells() -> Result<WasmPipelineMetrics, String> {
    let source = include_str!("../../../playground/frontend/src/examples/cells/cells.bn");

    let lower_started = Instant::now();
    let semantic = lower::try_lower_to_semantic(source, None, false)?;
    let lower_millis = lower_started.elapsed().as_millis();
    let exec_started = Instant::now();
    let exec = exec_ir::ExecProgram::from_semantic(&semantic);
    let exec_build_millis = exec_started.elapsed().as_millis();
    let lower_exec_millis = lower_millis.saturating_add(exec_build_millis);

    let mut runtime = runtime::WasmRuntime::new(0, 0, false);
    runtime.enable_incremental_diff();

    let init_started = Instant::now();
    let init_descriptor = runtime.init(&exec);
    let init_runtime_millis = init_started.elapsed().as_millis();
    let init_apply_started = Instant::now();
    let init_batch = runtime
        .decode_commands(init_descriptor)
        .map_err(|error| error.to_string())?;
    let init_metrics = batch_metrics(&init_batch);

    let mut state = FakeRenderState::default();
    let _ = apply_batch_and_root(&mut state, &init_batch)?;
    let init_decode_apply_millis = init_apply_started.elapsed().as_millis();
    let init_millis = init_runtime_millis.saturating_add(init_decode_apply_millis);
    let first_render_total_millis = lower_exec_millis.saturating_add(init_millis);

    let (
        edit_entry_millis,
        input_update_millis,
        commit_runtime_millis,
        commit_decode_apply_millis,
        a1_commit_batch,
    ) = dispatch_text_input_commit_with_state(&mut runtime, &mut state, 0, "7")?;
    let a1_commit_millis = edit_entry_millis
        .saturating_add(commit_runtime_millis)
        .saturating_add(commit_decode_apply_millis);
    let a1_commit_metrics = batch_metrics(&a1_commit_batch);

    let (
        dependent_edit_entry_millis,
        _dependent_input_update_millis,
        dependent_recompute_runtime_millis,
        dependent_recompute_decode_apply_millis,
        a2_recompute_batch,
    ) = dispatch_text_input_commit_with_state(&mut runtime, &mut state, 26, "20")?;
    let a2_recompute_millis = dependent_edit_entry_millis
        .saturating_add(dependent_recompute_runtime_millis)
        .saturating_add(dependent_recompute_decode_apply_millis);
    let a2_recompute_metrics = batch_metrics(&a2_recompute_batch);

    Ok(WasmPipelineMetrics {
        lower_millis,
        exec_build_millis,
        lower_exec_millis,
        init_runtime_millis,
        init_decode_apply_millis,
        init_millis,
        first_render_total_millis,
        edit_entry_millis,
        input_update_millis,
        commit_runtime_millis,
        commit_decode_apply_millis,
        a1_commit_millis,
        dependent_recompute_runtime_millis,
        dependent_recompute_decode_apply_millis,
        a2_recompute_millis,
        init_batch: init_metrics,
        a1_commit_batch: a1_commit_metrics,
        a2_recompute_batch: a2_recompute_metrics,
    })
}

pub fn cells_backend_metrics_snapshot() -> Result<CellsBackendMetricsReport, String> {
    Ok(CellsBackendMetricsReport {
        wasm: wasm_pipeline_metrics_for_cells()?,
    })
}

pub fn official_7guis_wasm_lowering_report() -> WasmLoweringReport {
    let examples = [
        (
            "temperature_converter",
            include_str!("../../../playground/frontend/src/examples/temperature_converter/temperature_converter.bn"),
        ),
        (
            "flight_booker",
            include_str!("../../../playground/frontend/src/examples/flight_booker/flight_booker.bn"),
        ),
        (
            "timer",
            include_str!("../../../playground/frontend/src/examples/timer/timer.bn"),
        ),
        (
            "crud",
            include_str!("../../../playground/frontend/src/examples/crud/crud.bn"),
        ),
        (
            "circle_drawer",
            include_str!("../../../playground/frontend/src/examples/circle_drawer/circle_drawer.bn"),
        ),
        (
            "cells",
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn"),
        ),
    ]
    .into_iter()
    .map(
        |(example_name, source): (&str, &str)| match try_lower_for_verification(source) {
        Ok(()) => WasmLoweringCheckResult {
            example_name: example_name.to_string(),
            passed: true,
            error: None,
        },
        Err(error) => WasmLoweringCheckResult {
            example_name: example_name.to_string(),
            passed: false,
            error: Some(error),
        },
    },
    )
    .collect();

    WasmLoweringReport { examples }
}

fn first_cells_row_summary(state: &FakeRenderState) -> serde_json::Value {
    fn direct_text(node: &boon_scene::UiNode) -> String {
        match &node.kind {
            boon_scene::UiNodeKind::Text { text } => text.clone(),
            boon_scene::UiNodeKind::Element { text, .. } => text.clone().unwrap_or_default(),
        }
    }

    fn visit<'a>(node: &'a boon_scene::UiNode, out: &mut Vec<&'a boon_scene::UiNode>) {
        out.push(node);
        for child in &node.children {
            visit(child, out);
        }
    }

    let Some(RenderRoot::UiTree(root)) = state.root() else {
        return serde_json::Value::Null;
    };

    let mut nodes = Vec::new();
    visit(root, &mut nodes);
    let mut row_one_seen = false;
    let mut row_cells = Vec::new();
    for node in nodes {
        let text = direct_text(node).trim().to_string();
        if text == "1" && !row_one_seen {
            row_one_seen = true;
            continue;
        }
        if row_one_seen {
            if text == "2" {
                break;
            }
            if !text.is_empty() {
                row_cells.push(text);
            }
        }
    }
    serde_json::json!(row_cells)
}

#[cfg(target_arch = "wasm32")]
fn update_browser_debug_state(
    runtime: &runtime::WasmRuntime,
    render_state: &FakeRenderState,
    label: &str,
) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let debug = serde_json::json!({
        "label": label,
        "runtime": runtime.debug_snapshot(),
        "firstCellsRow": first_cells_row_summary(render_state),
    });
    let Ok(debug_js) = serde_wasm_bindgen::to_value(&debug) else {
        return;
    };
    let _ = Reflect::set(
        window.as_ref(),
        &JsValue::from_str("__boonWasmDebug"),
        &debug_js,
    );
}

#[cfg(not(target_arch = "wasm32"))]
fn update_browser_debug_state(
    _runtime: &runtime::WasmRuntime,
    _render_state: &FakeRenderState,
    _label: &str,
) {
}

fn fnv1a_extend(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(1099511628211);
    }
}

fn wasm_persistence_key(source: &str, external_functions: Option<&[ExternalFunction]>) -> String {
    let mut hash = 14695981039346656037_u64;
    fnv1a_extend(&mut hash, source.as_bytes());
    if let Some(functions) = external_functions {
        for (name, params, body, alias) in functions {
            fnv1a_extend(&mut hash, name.as_bytes());
            for param in params {
                fnv1a_extend(&mut hash, param.as_bytes());
            }
            fnv1a_extend(&mut hash, format!("{body:?}").as_bytes());
            if let Some(alias) = alias {
                fnv1a_extend(&mut hash, alias.as_bytes());
            }
        }
    }
    format!("{WASM_PERSISTENCE_KEY_PREFIX}{hash:016x}")
}

#[cfg(target_arch = "wasm32")]
fn browser_current_path() -> Option<String> {
    let window = web_sys::window()?;
    let path = window.location().pathname().ok()?;
    Some(if path.is_empty() {
        "/".to_string()
    } else {
        path
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn browser_current_path() -> Option<String> {
    None
}

#[cfg(target_arch = "wasm32")]
fn sync_browser_route_from_runtime(runtime: &runtime::WasmRuntime, push: bool) {
    let Some(route) = runtime.primary_route_path() else {
        return;
    };
    let Some(window) = web_sys::window() else {
        return;
    };
    let location = window.location();
    let current_path = location.pathname().ok().unwrap_or_default();
    let next_path = if route.is_empty() {
        "/".to_string()
    } else {
        route
    };
    if current_path == next_path {
        return;
    }
    let search = location.search().ok().unwrap_or_default();
    let hash = location.hash().ok().unwrap_or_default();
    let url = format!("{next_path}{search}{hash}");
    let Ok(history) = window.history() else {
        return;
    };
    let _ = if push {
        history.push_state_with_url(&JsValue::NULL, "", Some(&url))
    } else {
        history.replace_state_with_url(&JsValue::NULL, "", Some(&url))
    };
}

#[cfg(not(target_arch = "wasm32"))]
fn sync_browser_route_from_runtime(_runtime: &runtime::WasmRuntime, _push: bool) {}

#[cfg(target_arch = "wasm32")]
fn browser_local_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

#[cfg(not(target_arch = "wasm32"))]
fn browser_local_storage() -> Option<()> {
    None
}

#[cfg(target_arch = "wasm32")]
fn load_persisted_wasm_state(key: &str) -> Option<PersistedWasmState> {
    let storage = browser_local_storage()?;
    let value = storage.get_item(key).ok().flatten()?;
    serde_json::from_str(&value).ok()
}

#[cfg(not(target_arch = "wasm32"))]
fn load_persisted_wasm_state(_key: &str) -> Option<PersistedWasmState> {
    None
}

#[cfg(target_arch = "wasm32")]
fn save_persisted_wasm_state(key: &str, runtime: &runtime::WasmRuntime) {
    let Some(storage) = browser_local_storage() else {
        return;
    };
    let state = PersistedWasmState {
        history: runtime.persisted_history(),
    };
    let Ok(json) = serde_json::to_string(&state) else {
        return;
    };
    let _ = storage.set_item(key, &json);
}

#[cfg(not(target_arch = "wasm32"))]
fn save_persisted_wasm_state(_key: &str, _runtime: &runtime::WasmRuntime) {}

struct WasmPreview {
    runtime: Rc<RefCell<runtime::WasmRuntime>>,
    render_state: Rc<RefCell<FakeRenderState>>,
    last_batch: Rc<RefCell<Option<RenderDiffBatch>>>,
    version: Mutable<u64>,
    persistence_key: Option<String>,
}

impl WasmPreview {
    fn new(
        source: &str,
        external_functions: Option<&[ExternalFunction]>,
        persistence_enabled: bool,
        exec: &exec_ir::ExecProgram,
    ) -> Option<Self> {
        let runtime = Rc::new(RefCell::new(runtime::bootstrap_runtime(
            source,
            external_functions.map_or(0, |functions| functions.len()),
            persistence_enabled,
        )));
        let render_state = Rc::new(RefCell::new(FakeRenderState::default()));
        let last_batch = Rc::new(RefCell::new(None));
        let preview = Self {
            runtime,
            render_state,
            last_batch,
            version: Mutable::new(0_u64),
            persistence_key: persistence_enabled
                .then(|| wasm_persistence_key(source, external_functions)),
        };
        let descriptor = preview.runtime.borrow_mut().init(exec);
        if preview.apply_descriptor(descriptor, "init") {
            preview.sync_route_from_browser();
            if !preview.restore_persisted_state() {
                return None;
            }
            preview.sync_browser_route(false);
            Some(preview)
        } else {
            None
        }
    }

    fn apply_descriptor(&self, descriptor: u64, label: &str) -> bool {
        if descriptor == 0 {
            wasm_browser_log(&format!("[wasm-preview] {label} descriptor=0"));
            wasm_set_debug_attr("data-boon-wasm-last-label", label);
            wasm_set_debug_attr("data-boon-wasm-last-descriptor", "0");
            update_browser_debug_state(&self.runtime.borrow(), &self.render_state.borrow(), "noop");
            return true;
        }
        let Ok(batch) = self.runtime.borrow().decode_commands(descriptor) else {
            wasm_browser_log(&format!(
                "[wasm-preview] {label} decode_commands failed for descriptor={descriptor}"
            ));
            wasm_set_debug_attr("data-boon-wasm-last-label", label);
            wasm_set_debug_attr("data-boon-wasm-last-descriptor", &descriptor.to_string());
            wasm_set_debug_attr("data-boon-wasm-last-error", "decode_commands_failed");
            return false;
        };
        *self.last_batch.borrow_mut() = Some(batch.clone());
        if self.render_state.borrow_mut().apply_batch(&batch).is_err() {
            wasm_browser_log(&format!(
                "[wasm-preview] {label} apply_batch failed for descriptor={descriptor}"
            ));
            wasm_set_debug_attr("data-boon-wasm-last-label", label);
            wasm_set_debug_attr("data-boon-wasm-last-descriptor", &descriptor.to_string());
            wasm_set_debug_attr("data-boon-wasm-last-error", "apply_batch_failed");
            return false;
        }
        wasm_browser_log(&format!(
            "[wasm-preview] {label} descriptor={descriptor} ops={} double_click_ports={} input_ports={} key_down_ports={}",
            batch.ops.len(),
            attached_port_count(&batch, UiEventKind::DoubleClick),
            attached_port_count(&batch, UiEventKind::Input),
            attached_port_count(&batch, UiEventKind::KeyDown),
        ));
        wasm_set_debug_attr("data-boon-wasm-last-label", label);
        wasm_set_debug_attr("data-boon-wasm-last-descriptor", &descriptor.to_string());
        wasm_set_debug_attr("data-boon-wasm-last-ops", &batch.ops.len().to_string());
        wasm_set_debug_attr(
            "data-boon-wasm-last-double-click-ports",
            &attached_port_count(&batch, UiEventKind::DoubleClick).to_string(),
        );
        wasm_set_debug_attr(
            "data-boon-wasm-last-input-ports",
            &attached_port_count(&batch, UiEventKind::Input).to_string(),
        );
        wasm_set_debug_attr(
            "data-boon-wasm-last-key-down-ports",
            &attached_port_count(&batch, UiEventKind::KeyDown).to_string(),
        );
        wasm_set_debug_attr("data-boon-wasm-last-error", "");
        update_browser_debug_state(&self.runtime.borrow(), &self.render_state.borrow(), label);
        self.sync_browser_route(label == "event");
        self.version.update(|current| current + 1);
        true
    }

    fn dispatch_events(&self, batch: UiEventBatch) -> bool {
        let bytes = abi::encode_ui_event_batch(&batch);
        let Ok(descriptor) = self.runtime.borrow_mut().dispatch_events(&bytes) else {
            return false;
        };
        let applied = self.apply_descriptor(descriptor, "event");
        if applied {
            self.save_persisted_state();
        }
        applied
    }

    fn apply_facts(&self, batch: UiFactBatch) -> bool {
        let bytes = abi::encode_ui_fact_batch(&batch);
        let Ok(descriptor) = self.runtime.borrow_mut().apply_facts(&bytes) else {
            return false;
        };
        let applied = self.apply_descriptor(descriptor, "fact");
        if applied {
            self.save_persisted_state();
        }
        applied
    }

    fn save_persisted_state(&self) {
        let Some(key) = self.persistence_key.as_deref() else {
            return;
        };
        save_persisted_wasm_state(key, &self.runtime.borrow());
    }

    fn restore_persisted_state(&self) -> bool {
        let Some(key) = self.persistence_key.as_deref() else {
            return true;
        };
        let Some(state) = load_persisted_wasm_state(key) else {
            return true;
        };
        for batch in state.history {
            match batch {
                PersistedWasmBatch::Events(batch) => {
                    let bytes = abi::encode_ui_event_batch(&batch);
                    let Ok(descriptor) = self.runtime.borrow_mut().dispatch_events(&bytes) else {
                        clear_wasm_persisted_states();
                        return false;
                    };
                    if !self.apply_descriptor(descriptor, "restore-event") {
                        clear_wasm_persisted_states();
                        return false;
                    }
                }
                PersistedWasmBatch::Facts(batch) => {
                    let bytes = abi::encode_ui_fact_batch(&batch);
                    let Ok(descriptor) = self.runtime.borrow_mut().apply_facts(&bytes) else {
                        clear_wasm_persisted_states();
                        return false;
                    };
                    if !self.apply_descriptor(descriptor, "restore-fact") {
                        clear_wasm_persisted_states();
                        return false;
                    }
                }
            }
        }
        self.save_persisted_state();
        true
    }

    fn sync_route_from_browser(&self) {
        let Some(path) = browser_current_path() else {
            return;
        };
        let descriptor = self.runtime.borrow_mut().set_route_path_and_render(&path);
        if descriptor != 0 {
            let _ = self.apply_descriptor(descriptor, "route-init");
        }
    }

    fn sync_browser_route(&self, push: bool) {
        sync_browser_route_from_runtime(&self.runtime.borrow(), push);
    }

    fn handlers(&self) -> boon_renderer_zoon::RenderInteractionHandlers {
        let runtime = self.runtime.clone();
        let render_state = self.render_state.clone();
        let last_batch = self.last_batch.clone();
        let version = self.version.clone();
        let persistence_key = self.persistence_key.clone();
        let apply_descriptor: Rc<dyn Fn(u64, &str)> = Rc::new(move |descriptor, label| {
            if descriptor == 0 {
                update_browser_debug_state(&runtime.borrow(), &render_state.borrow(), "noop");
                return;
            }
            let Ok(batch) = runtime.borrow().decode_commands(descriptor) else {
                return;
            };
            *last_batch.borrow_mut() = Some(batch.clone());
            if render_state.borrow_mut().apply_batch(&batch).is_err() {
                return;
            }
            update_browser_debug_state(&runtime.borrow(), &render_state.borrow(), label);
            sync_browser_route_from_runtime(&runtime.borrow(), label == "event");
            version.update(|current| current + 1);
        });

        let event_runtime = self.runtime.clone();
        let event_apply = apply_descriptor.clone();
        let event_handler = move |batch: UiEventBatch| {
            let raw_batch = batch.clone();
            let batch = {
                let runtime = event_runtime.borrow();
                sanitize_preview_event_batch(&runtime, batch)
            };
            let trace_text_event = raw_batch
                .events
                .iter()
                .any(|event| matches!(event.kind, UiEventKind::Input | UiEventKind::KeyDown));
            if raw_batch
                .events
                .iter()
                .any(|event| event.kind == UiEventKind::DoubleClick)
            {
                wasm_set_debug_attr(
                    "data-boon-wasm-last-raw-double-click-events",
                    &format!("{:?}", raw_batch.events),
                );
                wasm_browser_log(&format!(
                    "[wasm-preview] raw double_click events={:?}",
                    raw_batch.events
                ));
                wasm_set_debug_attr(
                    "data-boon-wasm-last-sanitized-double-click-events",
                    &format!("{:?}", batch.events),
                );
                wasm_browser_log(&format!(
                    "[wasm-preview] sanitized double_click events={:?}",
                    batch.events
                ));
            }
            if trace_text_event {
                wasm_browser_log(&format!(
                    "[wasm-preview] raw text events={:?}",
                    raw_batch.events
                ));
                wasm_browser_log(&format!(
                    "[wasm-preview] sanitized text events={:?}",
                    batch.events
                ));
            }
            let bytes = abi::encode_ui_event_batch(&batch);
            let Ok(descriptor) = event_runtime.borrow_mut().dispatch_events(&bytes) else {
                if raw_batch
                    .events
                    .iter()
                    .any(|event| event.kind == UiEventKind::DoubleClick)
                {
                    wasm_set_debug_attr(
                        "data-boon-wasm-last-error",
                        "dispatch_events_failed_for_double_click",
                    );
                    wasm_browser_log("[wasm-preview] dispatch_events failed for double_click batch");
                }
                if trace_text_event {
                    wasm_browser_log("[wasm-preview] dispatch_events failed for text batch");
                }
                return;
            };
            if raw_batch
                .events
                .iter()
                .any(|event| event.kind == UiEventKind::DoubleClick)
            {
                let snapshot = event_runtime.borrow().debug_snapshot();
                wasm_set_debug_attr(
                    "data-boon-wasm-last-double-click-dispatch-descriptor",
                    &descriptor.to_string(),
                );
                wasm_set_debug_attr(
                    "data-boon-wasm-last-active-actions",
                    &snapshot["activeActions"].to_string(),
                );
                wasm_set_debug_attr(
                    "data-boon-wasm-last-event-count",
                    &snapshot["eventCount"].to_string(),
                );
                wasm_set_debug_attr(
                    "data-boon-wasm-last-recent-events",
                    &snapshot["recentEvents"].to_string(),
                );
                wasm_set_debug_attr(
                    "data-boon-wasm-last-recent-actions",
                    &snapshot["recentActions"].to_string(),
                );
                wasm_set_debug_attr(
                    "data-boon-wasm-last-last-exec-bindings",
                    &snapshot["lastExecEventBindings"].to_string(),
                );
                wasm_set_debug_attr(
                    "data-boon-wasm-last-scalar-values",
                    &snapshot["scalarValues"].to_string(),
                );
                wasm_set_debug_attr(
                    "data-boon-wasm-last-text-values",
                    &snapshot["textValues"].to_string(),
                );
                wasm_set_debug_attr(
                    "data-boon-wasm-last-materialized-has-input",
                    &snapshot["materializedHasInput"].to_string(),
                );
                wasm_set_debug_attr(
                    "data-boon-wasm-last-editing-branches",
                    &snapshot["editingBranches"].to_string(),
                );
                wasm_browser_log(&format!(
                    "[wasm-preview] double_click descriptor={descriptor}"
                ));
            }
            if trace_text_event {
                let snapshot = event_runtime.borrow().debug_snapshot();
                wasm_browser_log(&format!(
                    "[wasm-preview] text descriptor={descriptor} last_event={} last_action={} overrides={} last_override={} text_values={} input_texts={}",
                    snapshot["lastEvent"],
                    snapshot["lastAction"],
                    snapshot["overridesCount"],
                    snapshot["lastOverride"],
                    snapshot["textValues"],
                    snapshot["inputTexts"],
                ));
            }
            event_apply(descriptor, "event");
            if let Some(key) = persistence_key.as_deref() {
                save_persisted_wasm_state(key, &event_runtime.borrow());
            }
        };

        let fact_runtime = self.runtime.clone();
        let fact_apply = apply_descriptor.clone();
        let persistence_key = self.persistence_key.clone();
        let fact_handler = move |batch: UiFactBatch| {
            let bytes = abi::encode_ui_fact_batch(&batch);
            let Ok(descriptor) = fact_runtime.borrow_mut().apply_facts(&bytes) else {
                return;
            };
            fact_apply(descriptor, "fact");
            if let Some(key) = persistence_key.as_deref() {
                save_persisted_wasm_state(key, &fact_runtime.borrow());
            }
        };

        boon_renderer_zoon::RenderInteractionHandlers::new(event_handler, fact_handler)
    }
}

fn sanitize_preview_event_batch(
    runtime: &runtime::WasmRuntime,
    mut batch: UiEventBatch,
) -> UiEventBatch {
    for event in &mut batch.events {
        if event.kind != UiEventKind::KeyDown
            || !runtime.should_strip_preview_keydown_text(event.target)
        {
            continue;
        }
        let Some(payload) = event.payload.as_deref() else {
            continue;
        };
        let Some((key, _)) = payload.split_once(runtime::KEYDOWN_TEXT_SEPARATOR) else {
            continue;
        };
        event.payload = Some(key.to_string());
    }
    batch
}

pub fn run_wasm(
    source: &str,
    external_functions: Option<
        &[(
            String,
            Vec<String>,
            boon::parser::static_expression::Spanned<boon::parser::static_expression::Expression>,
            Option<String>,
        )],
    >,
    persistence_enabled: bool,
) -> RawElOrText {
    let semantic =
        match lower::try_lower_to_semantic(source, external_functions, persistence_enabled) {
            Ok(semantic) => semantic,
            Err(error) => return error_element(&error),
        };
    let exec = exec_ir::ExecProgram::from_semantic(&semantic);
    let _summary = debug::summarize(&semantic, &exec);
    let Some(preview) = WasmPreview::new(source, external_functions, persistence_enabled, &exec)
    else {
        return missing_document_root();
    };
    let handlers = preview.handlers();
    El::new()
        .child_signal(preview.version.signal().map({
            let render_state = preview.render_state.clone();
            let handlers = handlers.clone();
            move |_| {
                Some(boon_renderer_zoon::render_fake_state_with_handlers(
                    &render_state.borrow(),
                    &handlers,
                ))
            }
        }))
        .unify()
}

fn error_element(msg: &str) -> RawElOrText {
    El::new()
        .s(Font::new().color(color!("LightCoral")))
        .child(msg.to_string())
        .unify()
}

#[cfg(test)]
mod tests {
    use super::*;
    use boon_scene::{
        EventPortId, NodeId, RenderOp, RenderRoot, UiEvent, UiEventBatch, UiEventKind, UiFact,
        UiFactBatch, UiFactKind, UiNode, UiNodeKind,
    };

    fn find_port(
        batch: &RenderDiffBatch,
        id: NodeId,
        kind: UiEventKind,
    ) -> Option<boon_scene::EventPortId> {
        batch.ops.iter().find_map(|op| match op {
            RenderOp::AttachEventPort {
                id: node_id,
                port,
                kind: event_kind,
            } if *node_id == id && *event_kind == kind => Some(*port),
            _ => None,
        })
    }

    fn find_nth_port(
        batch: &RenderDiffBatch,
        kind: UiEventKind,
        index: usize,
    ) -> Option<boon_scene::EventPortId> {
        batch
            .ops
            .iter()
            .filter_map(|op| match op {
                RenderOp::AttachEventPort {
                    port,
                    kind: event_kind,
                    ..
                } if *event_kind == kind => Some(*port),
                _ => None,
            })
            .nth(index)
    }

    fn find_first_tag<'a>(node: &'a UiNode, tag: &str) -> Option<&'a UiNode> {
        match &node.kind {
            UiNodeKind::Element { tag: node_tag, .. } if node_tag == tag => Some(node),
            _ => node
                .children
                .iter()
                .find_map(|child| find_first_tag(child, tag)),
        }
    }

    fn find_nth_tag<'a>(node: &'a UiNode, tag: &str, ordinal: usize) -> Option<&'a UiNode> {
        fn collect<'a>(node: &'a UiNode, tag: &str, out: &mut Vec<&'a UiNode>) {
            if matches!(&node.kind, UiNodeKind::Element { tag: node_tag, .. } if node_tag == tag) {
                out.push(node);
            }
            for child in &node.children {
                collect(child, tag, out);
            }
        }

        let mut matches = Vec::new();
        collect(node, tag, &mut matches);
        matches.into_iter().nth(ordinal)
    }

    fn tree_contains_text(node: &UiNode, expected: &str) -> bool {
        match &node.kind {
            UiNodeKind::Text { text } => text == expected,
            UiNodeKind::Element { text, .. } => {
                text.as_deref() == Some(expected)
                    || node
                        .children
                        .iter()
                        .any(|child| tree_contains_text(child, expected))
            }
        }
    }

    fn current_root(preview: &WasmPreview) -> UiNode {
        let render_state = preview.render_state.borrow();
        let Some(RenderRoot::UiTree(root)) = render_state.root() else {
            panic!("expected preview ui root");
        };
        root.clone()
    }

    fn first_cells_row_values(preview: &WasmPreview) -> Vec<String> {
        super::first_cells_row_summary(&preview.render_state.borrow())
            .as_array()
            .expect("cells summary should be an array")
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .expect("cells summary entry should be text")
                    .to_string()
            })
            .collect()
    }

    fn property_value<'a>(
        batch: &'a RenderDiffBatch,
        node_id: NodeId,
        name: &str,
    ) -> Option<&'a str> {
        batch.ops.iter().find_map(|op| match op {
            RenderOp::SetProperty {
                id,
                name: property_name,
                value: Some(value),
            } if *id == node_id && property_name == name => Some(value.as_str()),
            _ => None,
        })
    }

    fn find_port_in_state(
        node: &UiNode,
        state: &FakeRenderState,
        id: NodeId,
        kind: UiEventKind,
    ) -> Option<EventPortId> {
        if node.id == id {
            return state
                .event_ports_for(id)
                .into_iter()
                .find_map(|(port, event_kind)| (event_kind == kind).then_some(port));
        }
        node.children
            .iter()
            .find_map(|child| find_port_in_state(child, state, id, kind.clone()))
    }

    fn count_ports_in_state(node: &UiNode, state: &FakeRenderState, kind: UiEventKind) -> usize {
        let here = state
            .event_ports_for(node.id)
            .into_iter()
            .filter(|(_, event_kind)| *event_kind == kind)
            .count();
        here + node
            .children
            .iter()
            .map(|child| count_ports_in_state(child, state, kind.clone()))
            .sum::<usize>()
    }

    fn find_nth_port_target_in_state(
        node: &UiNode,
        state: &FakeRenderState,
        kind: UiEventKind,
        ordinal: usize,
    ) -> Option<NodeId> {
        fn collect(
            node: &UiNode,
            state: &FakeRenderState,
            kind: &UiEventKind,
            ids: &mut Vec<NodeId>,
        ) {
            if state
                .event_ports_for(node.id)
                .into_iter()
                .any(|(_, event_kind)| &event_kind == kind)
            {
                ids.push(node.id);
            }
            for child in &node.children {
                collect(child, state, kind, ids);
            }
        }

        let mut ids = Vec::new();
        collect(node, state, &kind, &mut ids);
        ids.into_iter().nth(ordinal)
    }

    #[test]
    fn preview_pipeline_todo_mvc_real_file_renders_input_without_error() {
        let source =
            include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let semantic = lower::lower_to_semantic(source, None, false);
        let exec = exec_ir::ExecProgram::from_semantic(&semantic);
        let preview =
            WasmPreview::new(source, None, false, &exec).expect("preview should initialize");

        let root = current_root(&preview);
        assert!(
            find_first_tag(&root, "input").is_some(),
            "todo_mvc preview should render the new todo input"
        );
        assert!(
            !tree_contains_text(&root, "detect_scalar_plan"),
            "todo_mvc preview should not render a Wasm lowering error"
        );
    }

    #[test]
    fn preview_pipeline_cells_fact_rerender_then_keydown_commits_value() {
        let source =
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn");
        let semantic = lower::lower_to_semantic(source, None, false);
        let exec = exec_ir::ExecProgram::from_semantic(&semantic);
        let preview =
            WasmPreview::new(source, None, false, &exec).expect("preview should initialize");

        let init_batch = preview
            .last_batch
            .borrow()
            .clone()
            .expect("init batch should exist");
        let double_click_port = find_nth_port(&init_batch, UiEventKind::DoubleClick, 0)
            .expect("A1 should expose DoubleClick");

        assert!(preview.dispatch_events(UiEventBatch {
            events: vec![UiEvent {
                target: double_click_port,
                kind: UiEventKind::DoubleClick,
                payload: None,
            }],
        }));

        let edit_batch = preview
            .last_batch
            .borrow()
            .clone()
            .expect("edit batch should exist");
        let root = current_root(&preview);
        let input = find_first_tag(&root, "input").expect("edit mode should render input");

        assert!(preview.apply_facts(UiFactBatch {
            facts: vec![UiFact {
                id: input.id,
                kind: UiFactKind::DraftText("7".to_string()),
            }],
        }));

        let fact_batch = preview
            .last_batch
            .borrow()
            .clone()
            .expect("fact batch should exist");
        let root = current_root(&preview);
        let rerendered_input =
            find_first_tag(&root, "input").expect("fact rerender should keep input");
        let key_down_port = {
            let render_state = preview.render_state.borrow();
            find_port_in_state(
                &root,
                &render_state,
                rerendered_input.id,
                UiEventKind::KeyDown,
            )
            .or_else(|| find_port(&fact_batch, rerendered_input.id, UiEventKind::KeyDown))
            .expect("rerendered input should expose KeyDown")
        };

        assert!(preview.dispatch_events(UiEventBatch {
            events: vec![UiEvent {
                target: key_down_port,
                kind: UiEventKind::KeyDown,
                payload: Some(format!("Enter{}7", runtime::KEYDOWN_TEXT_SEPARATOR)),
            }],
        }));

        let commit_batch = preview
            .last_batch
            .borrow()
            .clone()
            .expect("commit batch should exist");
        let root = current_root(&preview);
        let _ = commit_batch;
        assert!(find_first_tag(&root, "input").is_none());
        assert_eq!(
            &first_cells_row_values(&preview)[..3],
            ["7".to_string(), "17".to_string(), "32".to_string()]
        );
        let debug = preview.runtime.borrow().debug_snapshot();
        assert_eq!(debug["overridesCount"].as_u64(), Some(1));
        assert_eq!(debug["lastOverride"]["row"].as_i64(), Some(1));
        assert_eq!(debug["lastOverride"]["column"].as_i64(), Some(1));
        assert_eq!(debug["lastOverride"]["text"].as_str(), Some("7"));
    }

    #[test]
    fn preview_pipeline_temperature_converter_input_updates_reciprocal_value() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/temperature_converter/temperature_converter.bn"
        );
        let semantic = lower::lower_to_semantic(source, None, false);
        let exec = exec_ir::ExecProgram::from_semantic(&semantic);
        let preview =
            WasmPreview::new(source, None, false, &exec).expect("preview should initialize");
        let handlers = preview.handlers();

        let init_root = current_root(&preview);
        let celsius_input =
            find_nth_tag(&init_root, "input", 0).expect("should render Celsius input");
        let celsius_port = {
            let render_state = preview.render_state.borrow();
            find_port_in_state(
                &init_root,
                &render_state,
                celsius_input.id,
                UiEventKind::Input,
            )
            .expect("Celsius input should expose Input")
        };

        handlers.dispatch_fact_batch(UiFactBatch {
            facts: vec![UiFact {
                id: celsius_input.id,
                kind: UiFactKind::DraftText("0".to_string()),
            }],
        });
        handlers.dispatch_event_batch(UiEventBatch {
            events: vec![UiEvent {
                target: celsius_port,
                kind: UiEventKind::Input,
                payload: Some("0".to_string()),
            }],
        });

        let update_batch = preview
            .last_batch
            .borrow()
            .clone()
            .expect("update batch should exist");
        let updated_root = current_root(&preview);
        let updated_celsius =
            find_nth_tag(&updated_root, "input", 0).expect("updated Celsius input should render");
        let updated_fahrenheit = find_nth_tag(&updated_root, "input", 1)
            .expect("updated Fahrenheit input should render");

        assert_eq!(
            property_value(&update_batch, updated_fahrenheit.id, "value"),
            Some("32")
        );
        let _ = updated_celsius;
    }

    #[test]
    fn preview_pipeline_temperature_converter_reverse_input_updates_reciprocal_value() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/temperature_converter/temperature_converter.bn"
        );
        let semantic = lower::lower_to_semantic(source, None, false);
        let exec = exec_ir::ExecProgram::from_semantic(&semantic);
        let preview =
            WasmPreview::new(source, None, false, &exec).expect("preview should initialize");
        let handlers = preview.handlers();

        let init_root = current_root(&preview);
        let fahrenheit_input =
            find_nth_tag(&init_root, "input", 1).expect("should render Fahrenheit input");
        let fahrenheit_port = {
            let render_state = preview.render_state.borrow();
            find_port_in_state(
                &init_root,
                &render_state,
                fahrenheit_input.id,
                UiEventKind::Input,
            )
            .expect("Fahrenheit input should expose Input")
        };

        handlers.dispatch_fact_batch(UiFactBatch {
            facts: vec![UiFact {
                id: fahrenheit_input.id,
                kind: UiFactKind::DraftText("32".to_string()),
            }],
        });
        handlers.dispatch_event_batch(UiEventBatch {
            events: vec![UiEvent {
                target: fahrenheit_port,
                kind: UiEventKind::Input,
                payload: Some("32".to_string()),
            }],
        });

        let update_batch = preview
            .last_batch
            .borrow()
            .clone()
            .expect("update batch should exist");
        let updated_root = current_root(&preview);
        let updated_celsius =
            find_nth_tag(&updated_root, "input", 0).expect("updated Celsius input should render");
        let updated_fahrenheit = find_nth_tag(&updated_root, "input", 1)
            .expect("updated Fahrenheit input should render");

        assert_eq!(
            property_value(&update_batch, updated_celsius.id, "value"),
            Some("0")
        );
        let debug = preview.runtime.borrow().debug_snapshot();
        assert_eq!(
            debug["textValues"]["store.last_edited"].as_str(),
            Some("Fahrenheit")
        );
        assert_eq!(
            debug["textValues"]["store.fahrenheit_raw"].as_str(),
            Some("32")
        );
        let _ = updated_fahrenheit;
    }

    #[test]
    fn preview_pipeline_temperature_converter_switches_active_draft_binding_between_inputs() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/temperature_converter/temperature_converter.bn"
        );
        let semantic = lower::lower_to_semantic(source, None, false);
        let exec = exec_ir::ExecProgram::from_semantic(&semantic);
        let preview =
            WasmPreview::new(source, None, false, &exec).expect("preview should initialize");
        let handlers = preview.handlers();

        let init_root = current_root(&preview);
        let celsius_input =
            find_nth_tag(&init_root, "input", 0).expect("should render Celsius input");
        let fahrenheit_input =
            find_nth_tag(&init_root, "input", 1).expect("should render Fahrenheit input");
        let celsius_port = {
            let render_state = preview.render_state.borrow();
            find_port_in_state(
                &init_root,
                &render_state,
                celsius_input.id,
                UiEventKind::Input,
            )
            .expect("Celsius input should expose Input")
        };

        handlers.dispatch_fact_batch(UiFactBatch {
            facts: vec![UiFact {
                id: celsius_input.id,
                kind: UiFactKind::DraftText("1000".to_string()),
            }],
        });
        handlers.dispatch_event_batch(UiEventBatch {
            events: vec![UiEvent {
                target: celsius_port,
                kind: UiEventKind::Input,
                payload: Some("1000".to_string()),
            }],
        });

        let after_forward_root = current_root(&preview);
        let after_forward_fahrenheit = find_nth_tag(&after_forward_root, "input", 1)
            .expect("forward Fahrenheit input should render");
        let (fahrenheit_after_forward_id, fahrenheit_after_forward_port) = {
            let render_state = preview.render_state.borrow();
            (
                after_forward_fahrenheit.id,
                find_port_in_state(
                    &after_forward_root,
                    &render_state,
                    after_forward_fahrenheit.id,
                    UiEventKind::Input,
                )
                .expect("Fahrenheit input should still expose Input"),
            )
        };
        let after_forward_debug = preview.runtime.borrow().debug_snapshot();
        assert_eq!(
            after_forward_debug["activeDraftBinding"].as_str(),
            Some("store.elements.celsius_input")
        );
        assert_eq!(
            after_forward_debug["textValues"]["store.last_edited"].as_str(),
            Some("Celsius")
        );
        assert_eq!(
            after_forward_debug["textValues"]["store.celsius_raw"].as_str(),
            Some("1000")
        );

        handlers.dispatch_fact_batch(UiFactBatch {
            facts: vec![UiFact {
                id: fahrenheit_after_forward_id,
                kind: UiFactKind::DraftText("32".to_string()),
            }],
        });
        handlers.dispatch_event_batch(UiEventBatch {
            events: vec![UiEvent {
                target: fahrenheit_after_forward_port,
                kind: UiEventKind::Input,
                payload: Some("32".to_string()),
            }],
        });

        let final_batch = preview
            .last_batch
            .borrow()
            .clone()
            .expect("reverse update batch should exist");
        let final_root = current_root(&preview);
        let updated_celsius =
            find_nth_tag(&final_root, "input", 0).expect("updated Celsius input should render");
        assert_eq!(
            property_value(&final_batch, updated_celsius.id, "value"),
            Some("0")
        );
        let final_debug = preview.runtime.borrow().debug_snapshot();
        assert_eq!(
            final_debug["activeDraftBinding"].as_str(),
            Some("store.elements.fahrenheit_input")
        );
        assert_eq!(
            final_debug["textValues"]["store.last_edited"].as_str(),
            Some("Fahrenheit")
        );
        assert_eq!(
            final_debug["textValues"]["store.celsius_raw"].as_str(),
            Some("1000")
        );
        assert_eq!(
            final_debug["textValues"]["store.fahrenheit_raw"].as_str(),
            Some("32")
        );
    }

    #[test]
    fn preview_handler_cells_fact_rerender_then_keydown_without_input_keeps_original_value() {
        let source =
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn");
        let semantic = lower::lower_to_semantic(source, None, false);
        let exec = exec_ir::ExecProgram::from_semantic(&semantic);
        let preview =
            WasmPreview::new(source, None, false, &exec).expect("preview should initialize");
        let handlers = preview.handlers();

        let init_batch = preview
            .last_batch
            .borrow()
            .clone()
            .expect("init batch should exist");
        let double_click_port = find_nth_port(&init_batch, UiEventKind::DoubleClick, 0)
            .expect("A1 should expose DoubleClick");

        handlers.dispatch_event_batch(UiEventBatch {
            events: vec![UiEvent {
                target: double_click_port,
                kind: UiEventKind::DoubleClick,
                payload: None,
            }],
        });

        let edit_batch = preview
            .last_batch
            .borrow()
            .clone()
            .expect("edit batch should exist");
        let root = current_root(&preview);
        let input = find_first_tag(&root, "input").expect("edit mode should render input");

        handlers.dispatch_fact_batch(UiFactBatch {
            facts: vec![UiFact {
                id: input.id,
                kind: UiFactKind::DraftText("7".to_string()),
            }],
        });

        let fact_batch = preview
            .last_batch
            .borrow()
            .clone()
            .expect("fact batch should exist");
        let root = current_root(&preview);
        let rerendered_input =
            find_first_tag(&root, "input").expect("fact rerender should keep input");
        let key_down_port = {
            let render_state = preview.render_state.borrow();
            find_port_in_state(
                &root,
                &render_state,
                rerendered_input.id,
                UiEventKind::KeyDown,
            )
            .or_else(|| find_port(&fact_batch, rerendered_input.id, UiEventKind::KeyDown))
            .expect("rerendered input should expose KeyDown")
        };

        handlers.dispatch_event_batch(UiEventBatch {
            events: vec![UiEvent {
                target: key_down_port,
                kind: UiEventKind::KeyDown,
                payload: Some(format!("Enter{}7", runtime::KEYDOWN_TEXT_SEPARATOR)),
            }],
        });

        let commit_batch = preview
            .last_batch
            .borrow()
            .clone()
            .expect("commit batch should exist");
        let root = current_root(&preview);
        let _ = commit_batch;
        assert!(find_first_tag(&root, "input").is_none());
        assert_eq!(
            &first_cells_row_values(&preview)[..3],
            ["5".to_string(), "15".to_string(), "30".to_string()]
        );
    }

    #[test]
    fn preview_handler_cells_fact_input_then_keydown_commits_value() {
        let source =
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn");
        let semantic = lower::lower_to_semantic(source, None, false);
        let exec = exec_ir::ExecProgram::from_semantic(&semantic);
        let preview =
            WasmPreview::new(source, None, false, &exec).expect("preview should initialize");
        let handlers = preview.handlers();

        let init_batch = preview
            .last_batch
            .borrow()
            .clone()
            .expect("init batch should exist");
        let double_click_port = find_nth_port(&init_batch, UiEventKind::DoubleClick, 0)
            .expect("A1 should expose DoubleClick");

        handlers.dispatch_event_batch(UiEventBatch {
            events: vec![UiEvent {
                target: double_click_port,
                kind: UiEventKind::DoubleClick,
                payload: None,
            }],
        });

        let edit_batch = preview
            .last_batch
            .borrow()
            .clone()
            .expect("edit batch should exist");
        let root = current_root(&preview);
        let input = find_first_tag(&root, "input").expect("edit mode should render input");
        let render_state = preview.render_state.borrow();
        let input_port = find_port_in_state(&root, &render_state, input.id, UiEventKind::Input)
            .or_else(|| find_port(&edit_batch, input.id, UiEventKind::Input))
            .expect("edit input should expose Input");
        drop(render_state);

        handlers.dispatch_fact_batch(UiFactBatch {
            facts: vec![UiFact {
                id: input.id,
                kind: UiFactKind::DraftText("7".to_string()),
            }],
        });

        let fact_batch = preview
            .last_batch
            .borrow()
            .clone()
            .expect("fact batch should exist");
        let root = current_root(&preview);
        let rerendered_input =
            find_first_tag(&root, "input").expect("fact rerender should keep input");
        let render_state = preview.render_state.borrow();
        let rerendered_input_port = find_port_in_state(
            &root,
            &render_state,
            rerendered_input.id,
            UiEventKind::Input,
        )
        .or_else(|| find_port(&fact_batch, rerendered_input.id, UiEventKind::Input))
        .unwrap_or(input_port);
        drop(render_state);

        handlers.dispatch_event_batch(UiEventBatch {
            events: vec![UiEvent {
                target: rerendered_input_port,
                kind: UiEventKind::Input,
                payload: Some("7".to_string()),
            }],
        });

        let input_batch = preview
            .last_batch
            .borrow()
            .clone()
            .expect("input batch should exist");
        let root = current_root(&preview);
        let live_input = find_first_tag(&root, "input").expect("input batch should keep input");
        let render_state = preview.render_state.borrow();
        let key_down_port =
            find_port_in_state(&root, &render_state, live_input.id, UiEventKind::KeyDown)
                .or_else(|| find_port(&input_batch, live_input.id, UiEventKind::KeyDown))
                .expect("rerendered input should expose KeyDown");
        drop(render_state);

        handlers.dispatch_event_batch(UiEventBatch {
            events: vec![UiEvent {
                target: key_down_port,
                kind: UiEventKind::KeyDown,
                payload: Some(format!("Enter{}7", runtime::KEYDOWN_TEXT_SEPARATOR)),
            }],
        });

        let commit_batch = preview
            .last_batch
            .borrow()
            .clone()
            .expect("commit batch should exist");
        let root = current_root(&preview);
        let _ = commit_batch;
        assert!(find_first_tag(&root, "input").is_none());
        assert_eq!(
            &first_cells_row_values(&preview)[..3],
            ["7".to_string(), "17".to_string(), "32".to_string()]
        );
        assert!(tree_contains_text(&root, "7"));
        assert!(tree_contains_text(&root, "17"));
        assert!(tree_contains_text(&root, "32"));
    }

    #[test]
    fn preview_handler_cells_input_event_updates_edit_changed_text() {
        let source =
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn");
        let semantic = lower::lower_to_semantic(source, None, false);
        let exec = exec_ir::ExecProgram::from_semantic(&semantic);
        let preview =
            WasmPreview::new(source, None, false, &exec).expect("preview should initialize");
        let handlers = preview.handlers();

        let init_batch = preview
            .last_batch
            .borrow()
            .clone()
            .expect("init batch should exist");
        let double_click_port = find_nth_port(&init_batch, UiEventKind::DoubleClick, 0)
            .expect("A1 should expose DoubleClick");

        handlers.dispatch_event_batch(UiEventBatch {
            events: vec![UiEvent {
                target: double_click_port,
                kind: UiEventKind::DoubleClick,
                payload: None,
            }],
        });

        let edit_batch = preview
            .last_batch
            .borrow()
            .clone()
            .expect("edit batch should exist");
        let root = current_root(&preview);
        let input = find_first_tag(&root, "input").expect("edit mode should render input");
        let render_state = preview.render_state.borrow();
        let input_port = find_port_in_state(&root, &render_state, input.id, UiEventKind::Input)
            .or_else(|| find_port(&edit_batch, input.id, UiEventKind::Input))
            .expect("edit input should expose Input");
        drop(render_state);

        handlers.dispatch_event_batch(UiEventBatch {
            events: vec![UiEvent {
                target: input_port,
                kind: UiEventKind::Input,
                payload: Some("7".to_string()),
            }],
        });

        let debug = preview.runtime.borrow().debug_snapshot();
        assert_eq!(debug["textValues"]["edit_changed.text"].as_str(), Some("7"));
    }

    #[test]
    fn preview_handler_cells_edit_input_keeps_single_input_and_keydown_port_after_input() {
        let source =
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn");
        let semantic = lower::lower_to_semantic(source, None, false);
        let exec = exec_ir::ExecProgram::from_semantic(&semantic);
        let preview =
            WasmPreview::new(source, None, false, &exec).expect("preview should initialize");
        let handlers = preview.handlers();

        let init_batch = preview
            .last_batch
            .borrow()
            .clone()
            .expect("init batch should exist");
        let double_click_port = find_nth_port(&init_batch, UiEventKind::DoubleClick, 0)
            .expect("A1 should expose DoubleClick");

        handlers.dispatch_event_batch(UiEventBatch {
            events: vec![UiEvent {
                target: double_click_port,
                kind: UiEventKind::DoubleClick,
                payload: None,
            }],
        });

        let edit_batch = preview
            .last_batch
            .borrow()
            .clone()
            .expect("edit batch should exist");
        let root = current_root(&preview);
        let input = find_first_tag(&root, "input").expect("edit mode should render input");
        let render_state = preview.render_state.borrow();
        let input_port = find_port_in_state(&root, &render_state, input.id, UiEventKind::Input)
            .or_else(|| find_port(&edit_batch, input.id, UiEventKind::Input))
            .expect("edit input should expose Input");
        drop(render_state);

        handlers.dispatch_fact_batch(UiFactBatch {
            facts: vec![UiFact {
                id: input.id,
                kind: UiFactKind::DraftText("7".to_string()),
            }],
        });

        let fact_batch = preview
            .last_batch
            .borrow()
            .clone()
            .expect("fact batch should exist");
        let root = current_root(&preview);
        let rerendered_input =
            find_first_tag(&root, "input").expect("fact rerender should keep input");
        let render_state = preview.render_state.borrow();
        let rerendered_input_port = find_port_in_state(
            &root,
            &render_state,
            rerendered_input.id,
            UiEventKind::Input,
        )
        .or_else(|| find_port(&fact_batch, rerendered_input.id, UiEventKind::Input))
        .unwrap_or(input_port);
        drop(render_state);

        handlers.dispatch_event_batch(UiEventBatch {
            events: vec![UiEvent {
                target: rerendered_input_port,
                kind: UiEventKind::Input,
                payload: Some("7".to_string()),
            }],
        });

        let root = current_root(&preview);
        let live_input = find_first_tag(&root, "input").expect("input batch should keep input");
        let render_state = preview.render_state.borrow();
        assert_eq!(
            count_ports_in_state(live_input, &render_state, UiEventKind::Input),
            1
        );
        assert_eq!(
            count_ports_in_state(live_input, &render_state, UiEventKind::KeyDown),
            1
        );
    }

    #[test]
    fn preview_pipeline_cells_commit_uses_incremental_batch_and_preserves_grid_ports() {
        let source =
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn");
        let semantic = lower::lower_to_semantic(source, None, false);
        let exec = exec_ir::ExecProgram::from_semantic(&semantic);
        let preview =
            WasmPreview::new(source, None, false, &exec).expect("preview should initialize");

        let init_batch = preview
            .last_batch
            .borrow()
            .clone()
            .expect("init batch should exist");
        assert!(
            init_batch
                .ops
                .iter()
                .any(|op| matches!(op, RenderOp::ReplaceRoot(RenderRoot::UiTree(_)))),
            "initial preview render should bootstrap with ReplaceRoot",
        );
        let double_click_port = find_nth_port(&init_batch, UiEventKind::DoubleClick, 0)
            .expect("A1 should expose DoubleClick");

        assert!(preview.dispatch_events(UiEventBatch {
            events: vec![UiEvent {
                target: double_click_port,
                kind: UiEventKind::DoubleClick,
                payload: None,
            }],
        }));

        let root = current_root(&preview);
        let input = find_first_tag(&root, "input").expect("edit mode should render input");
        assert!(preview.apply_facts(UiFactBatch {
            facts: vec![UiFact {
                id: input.id,
                kind: UiFactKind::DraftText("7".to_string()),
            }],
        }));

        let fact_batch = preview
            .last_batch
            .borrow()
            .clone()
            .expect("fact batch should exist");
        let root = current_root(&preview);
        let rerendered_input =
            find_first_tag(&root, "input").expect("fact rerender should keep input");
        let key_down_port = {
            let render_state = preview.render_state.borrow();
            find_port_in_state(
                &root,
                &render_state,
                rerendered_input.id,
                UiEventKind::KeyDown,
            )
            .or_else(|| find_port(&fact_batch, rerendered_input.id, UiEventKind::KeyDown))
            .expect("rerendered input should expose KeyDown")
        };

        assert!(preview.dispatch_events(UiEventBatch {
            events: vec![UiEvent {
                target: key_down_port,
                kind: UiEventKind::KeyDown,
                payload: Some(format!("Enter{}7", runtime::KEYDOWN_TEXT_SEPARATOR)),
            }],
        }));

        let commit_batch = preview
            .last_batch
            .borrow()
            .clone()
            .expect("commit batch should exist");
        assert!(
            commit_batch
                .ops
                .iter()
                .all(|op| !matches!(op, RenderOp::ReplaceRoot(RenderRoot::UiTree(_)))),
            "commit batch should be incremental, not ReplaceRoot",
        );
        assert!(
            commit_batch.ops.len() <= 12,
            "expected small incremental commit batch, got {} ops",
            commit_batch.ops.len(),
        );

        let root = current_root(&preview);
        let render_state = preview.render_state.borrow();
        assert_eq!(
            count_ports_in_state(&root, &render_state, UiEventKind::DoubleClick),
            26 * 100,
            "incremental preview batches should preserve the full cells event surface",
        );
        assert!(tree_contains_text(&root, "7"));
        assert!(tree_contains_text(&root, "17"));
        assert!(tree_contains_text(&root, "32"));
    }

    #[test]
    fn preview_pipeline_cells_commit_restores_same_edited_cell_identity() {
        let source =
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn");
        let semantic = lower::lower_to_semantic(source, None, false);
        let exec = exec_ir::ExecProgram::from_semantic(&semantic);
        let preview =
            WasmPreview::new(source, None, false, &exec).expect("preview should initialize");

        let initial_root = current_root(&preview);
        let initial_id = {
            let render_state = preview.render_state.borrow();
            find_nth_port_target_in_state(&initial_root, &render_state, UiEventKind::DoubleClick, 0)
                .expect("A1 should expose DoubleClick")
        };
        let init_batch = preview
            .last_batch
            .borrow()
            .clone()
            .expect("init batch should exist");
        let double_click_port = find_nth_port(&init_batch, UiEventKind::DoubleClick, 0)
            .expect("A1 should expose DoubleClick");

        assert!(preview.dispatch_events(UiEventBatch {
            events: vec![UiEvent {
                target: double_click_port,
                kind: UiEventKind::DoubleClick,
                payload: None,
            }],
        }));

        let root = current_root(&preview);
        let input = find_first_tag(&root, "input").expect("edit mode should render input");
        assert!(preview.apply_facts(UiFactBatch {
            facts: vec![UiFact {
                id: input.id,
                kind: UiFactKind::DraftText("7".to_string()),
            }],
        }));

        let fact_batch = preview
            .last_batch
            .borrow()
            .clone()
            .expect("fact batch should exist");
        let root = current_root(&preview);
        let rerendered_input =
            find_first_tag(&root, "input").expect("fact rerender should keep input");
        let key_down_port = {
            let render_state = preview.render_state.borrow();
            find_port_in_state(
                &root,
                &render_state,
                rerendered_input.id,
                UiEventKind::KeyDown,
            )
            .or_else(|| find_port(&fact_batch, rerendered_input.id, UiEventKind::KeyDown))
            .expect("rerendered input should expose KeyDown")
        };

        assert!(preview.dispatch_events(UiEventBatch {
            events: vec![UiEvent {
                target: key_down_port,
                kind: UiEventKind::KeyDown,
                payload: Some(format!("Enter{}7", runtime::KEYDOWN_TEXT_SEPARATOR)),
            }],
        }));

        let root = current_root(&preview);
        let render_state = preview.render_state.borrow();
        assert_eq!(find_first_tag(&root, "input"), None);
        assert_eq!(
            find_nth_port_target_in_state(&root, &render_state, UiEventKind::DoubleClick, 0),
            Some(initial_id),
            "A1 should regain the same display identity after commit",
        );
    }

    #[test]
    fn preview_pipeline_cells_cancel_restores_same_edited_cell_identity() {
        let source =
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn");
        let semantic = lower::lower_to_semantic(source, None, false);
        let exec = exec_ir::ExecProgram::from_semantic(&semantic);
        let preview =
            WasmPreview::new(source, None, false, &exec).expect("preview should initialize");

        let initial_root = current_root(&preview);
        let initial_id = {
            let render_state = preview.render_state.borrow();
            find_nth_port_target_in_state(&initial_root, &render_state, UiEventKind::DoubleClick, 0)
                .expect("A1 should expose DoubleClick")
        };
        let init_batch = preview
            .last_batch
            .borrow()
            .clone()
            .expect("init batch should exist");
        let double_click_port = find_nth_port(&init_batch, UiEventKind::DoubleClick, 0)
            .expect("A1 should expose DoubleClick");

        assert!(preview.dispatch_events(UiEventBatch {
            events: vec![UiEvent {
                target: double_click_port,
                kind: UiEventKind::DoubleClick,
                payload: None,
            }],
        }));

        let root = current_root(&preview);
        let input = find_first_tag(&root, "input").expect("edit mode should render input");
        assert!(preview.apply_facts(UiFactBatch {
            facts: vec![UiFact {
                id: input.id,
                kind: UiFactKind::DraftText("1234".to_string()),
            }],
        }));

        let fact_batch = preview
            .last_batch
            .borrow()
            .clone()
            .expect("fact batch should exist");
        let root = current_root(&preview);
        let rerendered_input =
            find_first_tag(&root, "input").expect("fact rerender should keep input");
        let key_down_port = {
            let render_state = preview.render_state.borrow();
            find_port_in_state(
                &root,
                &render_state,
                rerendered_input.id,
                UiEventKind::KeyDown,
            )
            .or_else(|| find_port(&fact_batch, rerendered_input.id, UiEventKind::KeyDown))
            .expect("rerendered input should expose KeyDown")
        };

        assert!(preview.dispatch_events(UiEventBatch {
            events: vec![UiEvent {
                target: key_down_port,
                kind: UiEventKind::KeyDown,
                payload: Some("Escape".to_string()),
            }],
        }));

        let root = current_root(&preview);
        let render_state = preview.render_state.borrow();
        assert_eq!(find_first_tag(&root, "input"), None);
        assert_eq!(
            find_nth_port_target_in_state(&root, &render_state, UiEventKind::DoubleClick, 0),
            Some(initial_id),
            "A1 should regain the same display identity after cancel",
        );
    }

    #[test]
    fn preview_pipeline_cells_blur_restores_same_edited_cell_identity() {
        let source =
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn");
        let semantic = lower::lower_to_semantic(source, None, false);
        let exec = exec_ir::ExecProgram::from_semantic(&semantic);
        let preview =
            WasmPreview::new(source, None, false, &exec).expect("preview should initialize");

        let initial_root = current_root(&preview);
        let initial_id = {
            let render_state = preview.render_state.borrow();
            find_nth_port_target_in_state(&initial_root, &render_state, UiEventKind::DoubleClick, 0)
                .expect("A1 should expose DoubleClick")
        };
        let init_batch = preview
            .last_batch
            .borrow()
            .clone()
            .expect("init batch should exist");
        let double_click_port = find_nth_port(&init_batch, UiEventKind::DoubleClick, 0)
            .expect("A1 should expose DoubleClick");

        assert!(preview.dispatch_events(UiEventBatch {
            events: vec![UiEvent {
                target: double_click_port,
                kind: UiEventKind::DoubleClick,
                payload: None,
            }],
        }));

        let root = current_root(&preview);
        let input = find_first_tag(&root, "input").expect("edit mode should render input");
        assert!(preview.apply_facts(UiFactBatch {
            facts: vec![UiFact {
                id: input.id,
                kind: UiFactKind::DraftText("2345".to_string()),
            }],
        }));

        let fact_batch = preview
            .last_batch
            .borrow()
            .clone()
            .expect("fact batch should exist");
        let root = current_root(&preview);
        let rerendered_input =
            find_first_tag(&root, "input").expect("fact rerender should keep input");
        let blur_port = {
            let render_state = preview.render_state.borrow();
            find_port_in_state(&root, &render_state, rerendered_input.id, UiEventKind::Blur)
                .or_else(|| find_port(&fact_batch, rerendered_input.id, UiEventKind::Blur))
                .expect("rerendered input should expose Blur")
        };

        assert!(preview.dispatch_events(UiEventBatch {
            events: vec![UiEvent {
                target: blur_port,
                kind: UiEventKind::Blur,
                payload: None,
            }],
        }));

        let root = current_root(&preview);
        let render_state = preview.render_state.borrow();
        assert_eq!(find_first_tag(&root, "input"), None);
        assert_eq!(
            find_nth_port_target_in_state(&root, &render_state, UiEventKind::DoubleClick, 0),
            Some(initial_id),
            "A1 should regain the same display identity after blur",
        );
    }
}
