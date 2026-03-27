use boon::zoon::*;
use boon_engine_actors_lite::{
    PUBLIC_PLAYGROUND_EXAMPLES as ACTORS_LITE_PUBLIC_PLAYGROUND_EXAMPLES,
    dispatch::classify_source as classify_actors_lite_source, run_actors_lite,
};
use boon_renderer_zoon::{
    FakeRenderState, RenderInteractionHandlers, render_fake_state_with_handlers,
};
use boon_scene::{EventPortId, NodeId, RenderDiffBatch, UiEventBatch, UiEventKind, UiFactBatch};
use host_view::{HostViewNode, HostViewTree, RenderBatchStats};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

mod cells;
pub mod debug;
mod depot;
mod host_view;
mod lower;
mod metrics;
mod parse;
mod runtime;
mod semantics;
mod todo;
mod toggle;
mod ui;

use cells::CellsState;
pub use debug::{
    DebugSnapshot, EngineStatus, HostCommandDebug, clear_runtime_state,
    last_debug_snapshot as factory_fabric_debug_snapshot,
    last_status as factory_fabric_status_snapshot, publish_error_state, publish_runtime_state,
};
pub use depot::{
    FunctionInstanceId, ListDepot, ListEntry, ListHandleId, ListItemId, ListMapInstanceTable,
    ListMapSync, ListStore, MapperSiteId, ScopeId, ViewNodeId,
};
pub use lower::{
    ButtonHoverProgram, ButtonHoverToClickProgram, CellsProgram, CompiledProgram, CounterProgram,
    SwitchHoldProgram, TodoProgram, compile_program,
};
pub use lower::{StaticDocumentProgram, compile_program_for_example};
pub use metrics::{
    CellsMetricsReport, CounterMetricsReport, FactoryFabricMetricsComparison,
    FactoryFabricMetricsReport, InteractionMetricsReport, LatencySummary, RuntimeCoreMetricsReport,
    factory_fabric_metrics_snapshot,
};
pub use runtime::{
    AppliedUpdate, BusSlotId, FabricLinkBinding, FabricListItem, FabricListScope, FabricSeq,
    FabricTick, FabricTrigger, FabricUpdate, MachineId, RegionId, RegionState, RuntimeCore,
};
pub use semantics::{LatestCandidate, select_latest};
use todo::TodoState;
use toggle::{ButtonHoverState, ButtonHoverToClickState, SwitchHoldState};
pub use ui::{FabricUiEventState, FabricUiStore};

pub const SUPPORTED_PLAYGROUND_EXAMPLES: &[&str] = ACTORS_LITE_PUBLIC_PLAYGROUND_EXAMPLES;
pub const MILESTONE_PLAYGROUND_EXAMPLES: &[&str] =
    &["counter", "todo_mvc", "cells", "cells_dynamic"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FabricValue {
    Number(i64),
    Text(String),
    Bool(bool),
    Skip,
}

impl FabricValue {
    #[must_use]
    pub const fn is_skip(&self) -> bool {
        matches!(self, Self::Skip)
    }
}

impl From<i64> for FabricValue {
    fn from(value: i64) -> Self {
        Self::Number(value)
    }
}

impl From<bool> for FabricValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<&str> for FabricValue {
    fn from(value: &str) -> Self {
        Self::Text(value.to_string())
    }
}

impl From<String> for FabricValue {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SinkPortId(pub u16);

#[derive(Debug, Clone)]
pub struct HostBatch {
    pub ui_events: UiEventBatch,
    pub ui_facts: UiFactBatch,
}

#[derive(Debug, Clone)]
pub struct HostFlushResult {
    pub render_diff: RenderDiffBatch,
    pub status: EngineStatus,
    pub debug: DebugSnapshot,
}

pub trait HostBridgeAdapter {}

#[derive(Debug, Default)]
pub struct NoopHostBridgeAdapter;

impl HostBridgeAdapter for NoopHostBridgeAdapter {}

struct CounterUiHandles {
    root: NodeId,
    counter_text: NodeId,
    increment_button: NodeId,
    increment_port: EventPortId,
}

pub struct FactoryFabricRunner {
    _host: Box<dyn HostBridgeAdapter>,
    runtime: RuntimeCore,
    last_flush_id: u64,
    view: HostViewTree,
    state: RunnerState,
}

enum RunnerState {
    StaticDocument {
        program: StaticDocumentProgram,
        root: NodeId,
    },
    Counter {
        program: CounterProgram,
        ui: CounterUiHandles,
        sink_values: BTreeMap<SinkPortId, FabricValue>,
        region: RegionId,
    },
    ButtonHover(ButtonHoverState),
    ButtonHoverToClick(ButtonHoverToClickState),
    SwitchHold(SwitchHoldState),
    Todo(TodoState),
    Cells(CellsState),
}

impl FactoryFabricRunner {
    pub fn new(compiled: CompiledProgram, host: Box<dyn HostBridgeAdapter>) -> Self {
        let mut runtime = RuntimeCore::new();
        let state = match compiled {
            CompiledProgram::StaticDocument(program) => RunnerState::StaticDocument {
                program,
                root: NodeId::new(),
            },
            CompiledProgram::Counter(program) => {
                let region = runtime.alloc_region();
                runtime.write_bus_value(BusSlotId(0), FabricValue::Number(program.initial_value));
                runtime.clear_dirty_bits();
                RunnerState::Counter {
                    sink_values: [(SinkPortId(0), FabricValue::Number(program.initial_value))]
                        .into_iter()
                        .collect(),
                    program,
                    ui: CounterUiHandles {
                        root: NodeId::new(),
                        counter_text: NodeId::new(),
                        increment_button: NodeId::new(),
                        increment_port: EventPortId::new(),
                    },
                    region,
                }
            }
            CompiledProgram::ButtonHover(program) => {
                RunnerState::ButtonHover(ButtonHoverState::new(program))
            }
            CompiledProgram::ButtonHoverToClick(program) => {
                RunnerState::ButtonHoverToClick(ButtonHoverToClickState::new(program))
            }
            CompiledProgram::SwitchHold(program) => {
                RunnerState::SwitchHold(SwitchHoldState::new(program))
            }
            CompiledProgram::TodoMvc(program) => {
                RunnerState::Todo(TodoState::new(program, &mut runtime))
            }
            CompiledProgram::Cells(program) => {
                RunnerState::Cells(CellsState::new(program, &mut runtime))
            }
        };
        Self {
            _host: host,
            runtime,
            last_flush_id: 0,
            view: HostViewTree::default(),
            state,
        }
    }

    pub fn initial_render(&mut self) -> HostFlushResult {
        self.last_flush_id += 1;
        self.view = self.view_tree();
        let (render_diff, render_stats) = self.view.into_render_batch_with_stats();
        HostFlushResult {
            render_diff,
            status: self.status(true),
            debug: self.debug_snapshot_with(None, 0, Vec::new(), render_stats),
        }
    }

    pub fn handle_host_batch(&mut self, batch: HostBatch) -> HostFlushResult {
        self.runtime.begin_host_batch();
        let mut dirty_closure_size = 0;

        for event in batch.ui_events.events {
            if self.handle_event(&event) {
                dirty_closure_size = 1;
            }
        }
        for fact in batch.ui_facts.facts {
            if self.handle_fact(&fact) {
                dirty_closure_size = 1;
            }
        }
        let dirty_sinks = self
            .runtime
            .dirty_bus_slots()
            .into_iter()
            .map(|slot| slot.0 as u16)
            .collect::<Vec<_>>();
        let next_view = self.view_tree();
        let (render_diff, render_stats) = self.view.diff_with_stats(&next_view);
        self.view = next_view;
        self.runtime.clear_dirty_bits();

        self.last_flush_id += 1;
        HostFlushResult {
            render_diff,
            status: self.status(true),
            debug: self.debug_snapshot_with(None, dirty_closure_size, dirty_sinks, render_stats),
        }
    }

    pub fn read_sink(&self, sink: SinkPortId) -> Option<&FabricValue> {
        match &self.state {
            RunnerState::StaticDocument { .. } => None,
            RunnerState::Counter { sink_values, .. } => sink_values.get(&sink),
            RunnerState::ButtonHover(..)
            | RunnerState::ButtonHoverToClick(..)
            | RunnerState::SwitchHold(..) => None,
            RunnerState::Todo(_) | RunnerState::Cells(_) => None,
        }
    }

    pub fn debug_snapshot(&self) -> DebugSnapshot {
        self.debug_snapshot_with(None, 0, Vec::new(), RenderBatchStats::default())
    }

    fn view_tree(&self) -> HostViewTree {
        match &self.state {
            RunnerState::StaticDocument { program, root } => HostViewTree::from_root(
                HostViewNode::element(*root, "div").with_text(program.text.clone()),
            ),
            RunnerState::Counter { program, ui, .. } => {
                HostViewTree::from_root(HostViewNode::element(ui.root, "div").with_children(vec![
                    HostViewNode::element(ui.counter_text, "span")
                        .with_text(self.current_counter().to_string()),
                    HostViewNode::element(ui.increment_button, "button")
                        .with_text(program.button_label.clone())
                        .with_event_port(ui.increment_port, UiEventKind::Click),
                ]))
            }
            RunnerState::ButtonHover(state) => state.view_tree(),
            RunnerState::ButtonHoverToClick(state) => state.view_tree(),
            RunnerState::SwitchHold(state) => state.view_tree(),
            RunnerState::Todo(todo) => todo.view_tree(),
            RunnerState::Cells(cells) => cells.view_tree(),
        }
    }

    fn current_counter(&self) -> i64 {
        match self.read_sink(SinkPortId(0)) {
            Some(FabricValue::Number(value)) => *value,
            Some(_) | None => match &self.state {
                RunnerState::StaticDocument { .. } => 0,
                RunnerState::Counter { program, .. } => program.initial_value,
                RunnerState::ButtonHover(..)
                | RunnerState::ButtonHoverToClick(..)
                | RunnerState::SwitchHold(..)
                | RunnerState::Todo(_)
                | RunnerState::Cells(_) => 0,
            },
        }
    }

    fn handle_event(&mut self, event: &boon_scene::UiEvent) -> bool {
        match &mut self.state {
            RunnerState::StaticDocument { .. } => false,
            RunnerState::Counter {
                program,
                ui,
                sink_values,
                region,
            } => {
                if event.target == ui.increment_port && event.kind == UiEventKind::Click {
                    self.runtime.schedule_region(*region);
                    let _ = self.runtime.pop_ready_region();
                    let current = match sink_values.get(&SinkPortId(0)) {
                        Some(FabricValue::Number(value)) => *value,
                        _ => program.initial_value,
                    };
                    let updated = current + program.increment_by;
                    sink_values.insert(SinkPortId(0), FabricValue::Number(updated));
                    self.runtime
                        .write_bus_value(BusSlotId(0), FabricValue::Number(updated));
                    true
                } else {
                    false
                }
            }
            RunnerState::ButtonHover(_) => false,
            RunnerState::ButtonHoverToClick(state) => state.handle_event(event),
            RunnerState::SwitchHold(state) => state.handle_event(event),
            RunnerState::Todo(todo) => todo.handle_event(&mut self.runtime, event),
            RunnerState::Cells(cells) => cells.handle_event(&mut self.runtime, event),
        }
    }

    fn handle_fact(&mut self, fact: &boon_scene::UiFact) -> bool {
        match &mut self.state {
            RunnerState::StaticDocument { .. } | RunnerState::Counter { .. } => false,
            RunnerState::ButtonHover(state) => state.handle_fact(fact),
            RunnerState::ButtonHoverToClick(_) | RunnerState::SwitchHold(_) => false,
            RunnerState::Todo(_) | RunnerState::Cells(_) => false,
        }
    }

    fn status(&self, supported: bool) -> EngineStatus {
        EngineStatus {
            engine: "FactoryFabric",
            supported,
            quiescent: true,
            last_flush_id: self.last_flush_id,
        }
    }

    fn debug_snapshot_with(
        &self,
        last_error: Option<String>,
        dirty_closure_size: usize,
        dirty_sinks: Vec<u16>,
        render_stats: RenderBatchStats,
    ) -> DebugSnapshot {
        DebugSnapshot {
            tick: self.runtime.tick(),
            quiescent: true,
            last_flush_id: self.last_flush_id,
            ready_regions: self.runtime.ready_regions(),
            regions: self
                .runtime
                .region_states()
                .into_iter()
                .map(|state| state.id)
                .collect(),
            dirty_sinks,
            host_commands: render_stats.host_commands,
            retained_node_creations: render_stats.retained_node_creations,
            retained_node_deletions: render_stats.retained_node_deletions,
            recreated_mapped_scopes: self.recreated_mapped_scopes(),
            dirty_closure_size,
            last_error,
        }
    }

    fn recreated_mapped_scopes(&self) -> usize {
        match &self.state {
            RunnerState::StaticDocument { .. } => 0,
            RunnerState::Counter { .. } => 0,
            RunnerState::ButtonHover(..)
            | RunnerState::ButtonHoverToClick(..)
            | RunnerState::SwitchHold(..) => 0,
            RunnerState::Todo(todo) => todo.last_recreated_mapped_scopes(),
            RunnerState::Cells(cells) => cells.last_recreated_mapped_scopes(),
        }
    }
}

struct FactoryFabricPreview {
    runner: Rc<RefCell<FactoryFabricRunner>>,
    render_state: Rc<RefCell<FakeRenderState>>,
    version: Mutable<u64>,
}

impl FactoryFabricPreview {
    fn new(compiled: CompiledProgram) -> Result<Self, String> {
        let runner = Rc::new(RefCell::new(FactoryFabricRunner::new(
            compiled,
            Box::<NoopHostBridgeAdapter>::default(),
        )));
        let render_state = Rc::new(RefCell::new(FakeRenderState::default()));
        let version = Mutable::new(0_u64);

        let initial = runner.borrow_mut().initial_render();
        render_state
            .borrow_mut()
            .apply_batch(&initial.render_diff)
            .map_err(|error| format!("FactoryFabric render error: {error:?}"))?;
        publish_runtime_state(initial.status, Some(initial.debug));

        Ok(Self {
            runner,
            render_state,
            version,
        })
    }

    fn handlers(&self) -> RenderInteractionHandlers {
        let runner = self.runner.clone();
        let render_state = self.render_state.clone();
        let version = self.version.clone();
        let runner_for_facts = runner.clone();
        let render_state_for_facts = render_state.clone();
        let version_for_facts = version.clone();

        RenderInteractionHandlers::new(
            move |batch| {
                let flush = runner.borrow_mut().handle_host_batch(HostBatch {
                    ui_events: batch,
                    ui_facts: UiFactBatch { facts: Vec::new() },
                });
                if render_state
                    .borrow_mut()
                    .apply_batch(&flush.render_diff)
                    .is_err()
                {
                    publish_error_state("FactoryFabric render error: failed to apply diff batch");
                    return;
                }
                publish_runtime_state(flush.status, Some(flush.debug));
                if !flush.render_diff.ops.is_empty() {
                    version.update(|value| value + 1);
                }
            },
            move |batch| {
                let flush = runner_for_facts.borrow_mut().handle_host_batch(HostBatch {
                    ui_events: UiEventBatch { events: Vec::new() },
                    ui_facts: batch,
                });
                if render_state_for_facts
                    .borrow_mut()
                    .apply_batch(&flush.render_diff)
                    .is_err()
                {
                    publish_error_state("FactoryFabric render error: failed to apply diff batch");
                    return;
                }
                publish_runtime_state(flush.status, Some(flush.debug));
                if !flush.render_diff.ops.is_empty() {
                    version_for_facts.update(|value| value + 1);
                }
            },
        )
    }
}

pub fn run_factory_fabric(example_name: &str, source: &str) -> RawElOrText {
    clear_runtime_state("FactoryFabric");
    let compiled = match compile_program_for_example(example_name, source) {
        Ok(compiled) => compiled,
        Err(error) => {
            if classify_actors_lite_source(source).is_ok() {
                publish_delegate_state(example_name);
                return run_actors_lite(source).unify();
            }
            publish_error_state(error.clone());
            return error_element(&error);
        }
    };
    let preview = match FactoryFabricPreview::new(compiled) {
        Ok(preview) => preview,
        Err(error) => {
            publish_error_state(error.clone());
            return error_element(&error);
        }
    };
    let handlers = preview.handlers();
    El::new()
        .child_signal(preview.version.signal().map({
            let render_state = preview.render_state.clone();
            let handlers = handlers.clone();
            move |_| {
                Some(render_fake_state_with_handlers(
                    &render_state.borrow(),
                    &handlers,
                ))
            }
        }))
        .unify()
}

fn error_element(message: &str) -> RawElOrText {
    El::new()
        .s(Font::new().color(color!("LightCoral")))
        .child(message.to_string())
        .unify()
}

fn publish_delegate_state(example_name: &str) {
    publish_runtime_state(
        EngineStatus {
            engine: "FactoryFabric",
            supported: true,
            quiescent: true,
            last_flush_id: 0,
        },
        Some(DebugSnapshot {
            tick: FabricTick(0),
            quiescent: true,
            last_flush_id: 0,
            ready_regions: Vec::new(),
            regions: Vec::new(),
            dirty_sinks: Vec::new(),
            host_commands: vec![HostCommandDebug {
                name: format!("actors-lite-compat:{example_name}"),
            }],
            retained_node_creations: 0,
            retained_node_deletions: 0,
            recreated_mapped_scopes: 0,
            dirty_closure_size: 0,
            last_error: None,
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::{
        FactoryFabricRunner, HostBatch, NoopHostBridgeAdapter, compile_program,
        compile_program_for_example,
    };
    use boon_renderer_zoon::FakeRenderState;
    use boon_scene::{
        RenderDiffBatch, RenderOp, RenderRoot, UiEvent, UiEventBatch, UiEventKind, UiFact,
        UiFactBatch, UiFactKind, UiNode, UiNodeKind,
    };

    fn counter_text_from_root(root: &UiNode) -> String {
        let UiNodeKind::Element {
            text: Some(text), ..
        } = &root.children[0].kind
        else {
            panic!("expected counter text node");
        };
        text.clone()
    }

    fn current_root(state: &FakeRenderState) -> UiNode {
        let Some(RenderRoot::UiTree(root)) = state.root() else {
            panic!("expected UI root");
        };
        root.clone()
    }

    fn button_port(batch: &RenderDiffBatch) -> boon_scene::EventPortId {
        batch
            .ops
            .iter()
            .find_map(|op| match op {
                RenderOp::AttachEventPort { port, kind, .. } if *kind == UiEventKind::Click => {
                    Some(*port)
                }
                _ => None,
            })
            .expect("expected click event port")
    }

    #[test]
    fn counter_click_updates_rendered_text_without_replacing_button() {
        let compiled = compile_program(include_str!(
            "../../../playground/frontend/src/examples/counter/counter.bn"
        ))
        .expect("counter should lower");
        let mut runner =
            FactoryFabricRunner::new(compiled, Box::<NoopHostBridgeAdapter>::default());
        let mut state = FakeRenderState::default();

        let initial = runner.initial_render();
        let click_port = button_port(&initial.render_diff);
        state
            .apply_batch(&initial.render_diff)
            .expect("initial render should apply");
        let initial_root = current_root(&state);
        let button_id = initial_root.children[1].id;
        assert_eq!(counter_text_from_root(&initial_root), "0");

        let update = runner.handle_host_batch(HostBatch {
            ui_events: UiEventBatch {
                events: vec![UiEvent {
                    target: click_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            },
            ui_facts: UiFactBatch { facts: Vec::new() },
        });
        state
            .apply_batch(&update.render_diff)
            .expect("update render should apply");
        let updated_root = current_root(&state);
        assert_eq!(counter_text_from_root(&updated_root), "1");
        assert_eq!(updated_root.children[1].id, button_id);
        assert_eq!(update.debug.retained_node_creations, 0);
        assert_eq!(update.debug.retained_node_deletions, 0);
        assert_eq!(update.debug.dirty_sinks, vec![0]);
        assert!(
            update
                .debug
                .host_commands
                .iter()
                .any(|command| command.name == "SetText")
        );
    }

    #[test]
    fn static_document_renders_minimal_text() {
        let compiled = compile_program_for_example(
            "minimal",
            include_str!("../../../playground/frontend/src/examples/minimal/minimal.bn"),
        )
        .expect("minimal should lower");
        let mut runner =
            FactoryFabricRunner::new(compiled, Box::<NoopHostBridgeAdapter>::default());
        let mut state = FakeRenderState::default();

        let initial = runner.initial_render();
        state
            .apply_batch(&initial.render_diff)
            .expect("initial render should apply");
        let root = current_root(&state);
        let UiNodeKind::Element {
            text: Some(text), ..
        } = &root.kind
        else {
            panic!("expected root text node");
        };
        assert_eq!(text, "123");
    }

    #[test]
    fn button_hover_outline_tracks_hovered_button_only() {
        let compiled = compile_program_for_example(
            "button_hover_test",
            include_str!(
                "../../../playground/frontend/src/examples/button_hover_test/button_hover_test.bn"
            ),
        )
        .expect("button_hover_test should lower");
        let mut runner =
            FactoryFabricRunner::new(compiled, Box::<NoopHostBridgeAdapter>::default());
        let mut state = FakeRenderState::default();

        let initial = runner.initial_render();
        state
            .apply_batch(&initial.render_diff)
            .expect("initial render should apply");
        let root = current_root(&state);
        let button_a = root.children[1].children[0].id;
        let button_b = root.children[1].children[1].id;

        let update = runner.handle_host_batch(HostBatch {
            ui_events: UiEventBatch { events: Vec::new() },
            ui_facts: UiFactBatch {
                facts: vec![UiFact {
                    id: button_b,
                    kind: UiFactKind::Hovered(true),
                }],
            },
        });
        state
            .apply_batch(&update.render_diff)
            .expect("hover update should apply");

        assert_eq!(state.style_value(button_a, "outline"), Some("none"));
        assert_eq!(
            state.style_value(button_b, "outline"),
            Some("2px solid oklch(0.6 0.2 250)")
        );
        assert_eq!(update.debug.retained_node_creations, 0);
        assert_eq!(update.debug.retained_node_deletions, 0);
    }

    #[test]
    fn button_hover_to_click_updates_state_label() {
        let compiled = compile_program_for_example(
            "button_hover_to_click_test",
            include_str!(
                "../../../playground/frontend/src/examples/button_hover_to_click_test/button_hover_to_click_test.bn"
            ),
        )
        .expect("button_hover_to_click_test should lower");
        let mut runner =
            FactoryFabricRunner::new(compiled, Box::<NoopHostBridgeAdapter>::default());
        let mut state = FakeRenderState::default();

        let initial = runner.initial_render();
        state
            .apply_batch(&initial.render_diff)
            .expect("initial render should apply");
        let click_port = initial
            .render_diff
            .ops
            .iter()
            .filter_map(|op| match op {
                RenderOp::AttachEventPort { port, kind, .. } if *kind == UiEventKind::Click => {
                    Some(*port)
                }
                _ => None,
            })
            .next()
            .expect("expected first example button click port");

        let update = runner.handle_host_batch(HostBatch {
            ui_events: UiEventBatch {
                events: vec![UiEvent {
                    target: click_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            },
            ui_facts: UiFactBatch { facts: Vec::new() },
        });
        state
            .apply_batch(&update.render_diff)
            .expect("click update should apply");

        let root = current_root(&state);
        let UiNodeKind::Element {
            text: Some(text), ..
        } = &root.children[2].kind
        else {
            panic!("expected state label node");
        };
        assert_eq!(text, "States - A: True, B: False, C: False");
    }

    #[test]
    fn switch_hold_preserves_counts_across_toggle() {
        let compiled = compile_program_for_example(
            "switch_hold_test",
            include_str!(
                "../../../playground/frontend/src/examples/switch_hold_test/switch_hold_test.bn"
            ),
        )
        .expect("switch_hold_test should lower");
        let mut runner =
            FactoryFabricRunner::new(compiled, Box::<NoopHostBridgeAdapter>::default());
        let mut state = FakeRenderState::default();

        let initial = runner.initial_render();
        state
            .apply_batch(&initial.render_diff)
            .expect("initial render should apply");
        let click_ports = initial
            .render_diff
            .ops
            .iter()
            .filter_map(|op| match op {
                RenderOp::AttachEventPort { port, kind, .. } if *kind == UiEventKind::Click => {
                    Some(*port)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(click_ports.len(), 3);

        for port in [
            click_ports[1],
            click_ports[0],
            click_ports[2],
            click_ports[0],
        ] {
            let update = runner.handle_host_batch(HostBatch {
                ui_events: UiEventBatch {
                    events: vec![UiEvent {
                        target: port,
                        kind: UiEventKind::Click,
                        payload: None,
                    }],
                },
                ui_facts: UiFactBatch { facts: Vec::new() },
            });
            state
                .apply_batch(&update.render_diff)
                .expect("switch hold update should apply");
        }

        let root = current_root(&state);
        let UiNodeKind::Element {
            text: Some(active_label),
            ..
        } = &root.children[0].kind
        else {
            panic!("expected active label node");
        };
        let UiNodeKind::Element {
            text: Some(count_label),
            ..
        } = &root.children[2].kind
        else {
            panic!("expected count label node");
        };
        assert_eq!(active_label, "Showing: Item A");
        assert_eq!(count_label, "Item A clicks: 1");
    }
}
