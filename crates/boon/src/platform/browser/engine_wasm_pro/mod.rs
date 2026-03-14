//! Wasm backend.

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
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsValue;
#[cfg(target_arch = "wasm32")]
use zoon::js_sys::Reflect;
use zoon::*;

use super::{BackendBatchMetrics, WasmPipelineMetrics};

type ExternalFunction = (
    String,
    Vec<String>,
    crate::parser::static_expression::Spanned<crate::parser::static_expression::Expression>,
    Option<String>,
);

fn ui_text_content(node: &UiNode) -> Option<&str> {
    match &node.kind {
        UiNodeKind::Text { text } => Some(text.as_str()),
        UiNodeKind::Element { text, .. } => text.as_deref(),
    }
}

fn ui_node_count(node: &UiNode) -> usize {
    1 + node.children.iter().map(ui_node_count).sum::<usize>()
}

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
    runtime: &mut runtime::WasmProRuntime,
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
    runtime: &mut runtime::WasmProRuntime,
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

pub(crate) fn clear_wasm_persisted_states() {}

pub(super) fn wasm_pipeline_metrics_for_cells() -> Result<WasmPipelineMetrics, String> {
    let source = include_str!("../../../../../../playground/frontend/src/examples/cells/cells.bn");

    let lower_started = Instant::now();
    let semantic = lower::lower_to_semantic(source, None, false);
    let lower_millis = lower_started.elapsed().as_millis();
    let exec_started = Instant::now();
    let exec = exec_ir::ExecProgram::from_semantic(&semantic);
    let exec_build_millis = exec_started.elapsed().as_millis();
    let lower_exec_millis = lower_millis.saturating_add(exec_build_millis);

    let mut runtime = runtime::WasmProRuntime::new(0, 0, false);
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
    runtime: &runtime::WasmProRuntime,
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
        &JsValue::from_str("__boonWasmProDebug"),
        &debug_js,
    );
}

#[cfg(not(target_arch = "wasm32"))]
fn update_browser_debug_state(
    _runtime: &runtime::WasmProRuntime,
    _render_state: &FakeRenderState,
    _label: &str,
) {
}

struct WasmProPreview {
    runtime: Rc<RefCell<runtime::WasmProRuntime>>,
    render_state: Rc<RefCell<FakeRenderState>>,
    last_batch: Rc<RefCell<Option<RenderDiffBatch>>>,
    version: Mutable<u64>,
}

impl WasmProPreview {
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
        };
        let descriptor = preview.runtime.borrow_mut().init(exec);
        if preview.apply_descriptor(descriptor, "init") {
            Some(preview)
        } else {
            None
        }
    }

    fn apply_descriptor(&self, descriptor: u64, label: &str) -> bool {
        if descriptor == 0 {
            update_browser_debug_state(&self.runtime.borrow(), &self.render_state.borrow(), "noop");
            return true;
        }
        let Ok(batch) = self.runtime.borrow().decode_commands(descriptor) else {
            return false;
        };
        *self.last_batch.borrow_mut() = Some(batch.clone());
        if self.render_state.borrow_mut().apply_batch(&batch).is_err() {
            return false;
        }
        update_browser_debug_state(&self.runtime.borrow(), &self.render_state.borrow(), label);
        self.version.update(|current| current + 1);
        true
    }

    fn dispatch_events(&self, batch: UiEventBatch) -> bool {
        let bytes = abi::encode_ui_event_batch(&batch);
        let Ok(descriptor) = self.runtime.borrow_mut().dispatch_events(&bytes) else {
            return false;
        };
        self.apply_descriptor(descriptor, "event")
    }

    fn apply_facts(&self, batch: UiFactBatch) -> bool {
        let bytes = abi::encode_ui_fact_batch(&batch);
        let Ok(descriptor) = self.runtime.borrow_mut().apply_facts(&bytes) else {
            return false;
        };
        self.apply_descriptor(descriptor, "fact")
    }

    fn handlers(&self) -> boon_renderer_zoon::RenderInteractionHandlers {
        let runtime = self.runtime.clone();
        let render_state = self.render_state.clone();
        let last_batch = self.last_batch.clone();
        let version = self.version.clone();
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
            version.update(|current| current + 1);
        });

        let event_runtime = self.runtime.clone();
        let event_apply = apply_descriptor.clone();
        let event_handler = move |batch: UiEventBatch| {
            let bytes = abi::encode_ui_event_batch(&batch);
            let Ok(descriptor) = event_runtime.borrow_mut().dispatch_events(&bytes) else {
                return;
            };
            event_apply(descriptor, "event");
        };

        let fact_runtime = self.runtime.clone();
        let fact_apply = apply_descriptor.clone();
        let fact_handler = move |batch: UiFactBatch| {
            let bytes = abi::encode_ui_fact_batch(&batch);
            let Ok(descriptor) = fact_runtime.borrow_mut().apply_facts(&bytes) else {
                return;
            };
            fact_apply(descriptor, "fact");
        };

        boon_renderer_zoon::RenderInteractionHandlers::new(event_handler, fact_handler)
    }
}

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
    let semantic = lower::lower_to_semantic(source, external_functions, persistence_enabled);
    let exec = exec_ir::ExecProgram::from_semantic(&semantic);
    let _summary = debug::summarize(&semantic, &exec);
    let Some(preview) = WasmProPreview::new(source, external_functions, persistence_enabled, &exec)
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

    fn current_root(preview: &WasmProPreview) -> UiNode {
        let render_state = preview.render_state.borrow();
        let Some(RenderRoot::UiTree(root)) = render_state.root() else {
            panic!("expected preview ui root");
        };
        root.clone()
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
    fn preview_pipeline_cells_fact_rerender_then_keydown_commits_value() {
        let source =
            include_str!("../../../../../../playground/frontend/src/examples/cells/cells.bn");
        let semantic = lower::lower_to_semantic(source, None, false);
        let exec = exec_ir::ExecProgram::from_semantic(&semantic);
        let preview =
            WasmProPreview::new(source, None, false, &exec).expect("preview should initialize");

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
        assert!(tree_contains_text(&root, "7"));
        assert!(tree_contains_text(&root, "17"));
        assert!(tree_contains_text(&root, "32"));
    }

    #[test]
    fn preview_handler_cells_fact_rerender_then_keydown_commits_value() {
        let source =
            include_str!("../../../../../../playground/frontend/src/examples/cells/cells.bn");
        let semantic = lower::lower_to_semantic(source, None, false);
        let exec = exec_ir::ExecProgram::from_semantic(&semantic);
        let preview =
            WasmProPreview::new(source, None, false, &exec).expect("preview should initialize");
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
        assert!(tree_contains_text(&root, "7"));
        assert!(tree_contains_text(&root, "17"));
        assert!(tree_contains_text(&root, "32"));
    }

    #[test]
    fn preview_handler_cells_fact_input_then_keydown_commits_value() {
        let source =
            include_str!("../../../../../../playground/frontend/src/examples/cells/cells.bn");
        let semantic = lower::lower_to_semantic(source, None, false);
        let exec = exec_ir::ExecProgram::from_semantic(&semantic);
        let preview =
            WasmProPreview::new(source, None, false, &exec).expect("preview should initialize");
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
        assert!(tree_contains_text(&root, "7"));
        assert!(tree_contains_text(&root, "17"));
        assert!(tree_contains_text(&root, "32"));
    }

    #[test]
    fn preview_pipeline_cells_commit_uses_incremental_batch_and_preserves_grid_ports() {
        let source =
            include_str!("../../../../../../playground/frontend/src/examples/cells/cells.bn");
        let semantic = lower::lower_to_semantic(source, None, false);
        let exec = exec_ir::ExecProgram::from_semantic(&semantic);
        let preview =
            WasmProPreview::new(source, None, false, &exec).expect("preview should initialize");

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
            include_str!("../../../../../../playground/frontend/src/examples/cells/cells.bn");
        let semantic = lower::lower_to_semantic(source, None, false);
        let exec = exec_ir::ExecProgram::from_semantic(&semantic);
        let preview =
            WasmProPreview::new(source, None, false, &exec).expect("preview should initialize");

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
            include_str!("../../../../../../playground/frontend/src/examples/cells/cells.bn");
        let semantic = lower::lower_to_semantic(source, None, false);
        let exec = exec_ir::ExecProgram::from_semantic(&semantic);
        let preview =
            WasmProPreview::new(source, None, false, &exec).expect("preview should initialize");

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
            include_str!("../../../../../../playground/frontend/src/examples/cells/cells.bn");
        let semantic = lower::lower_to_semantic(source, None, false);
        let exec = exec_ir::ExecProgram::from_semantic(&semantic);
        let preview =
            WasmProPreview::new(source, None, false, &exec).expect("preview should initialize");

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
