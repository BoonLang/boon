use crate::bridge::HostInput;
use crate::ids::ActorId;
use crate::interactive_preview::{
    InteractivePreviewState, decode_ui_event, decode_ui_fact, encode_ui_event, encode_ui_fact,
};
use crate::ir::{MirrorCellId, RetainedNodeKey, SourcePortId};
use crate::preview_runtime::PreviewRuntime;
use crate::runtime::Msg;
use boon_renderer_zoon::FakeRenderState;
use boon_scene::{
    NodeId, RenderOp, RenderRoot, UiEventBatch, UiEventKind, UiFactBatch, UiFactKind, UiNode,
};

#[derive(Debug)]
pub(crate) struct RuntimeBackedPreviewState<Action, FactTarget> {
    runtime: PreviewRuntime,
    host_actor: ActorId,
    ui: InteractivePreviewState<Action, FactTarget>,
    input_scratch: Vec<HostInput>,
}

impl<Action, FactTarget> Default for RuntimeBackedPreviewState<Action, FactTarget>
where
    Action: Clone,
    FactTarget: Clone,
{
    fn default() -> Self {
        let mut runtime = PreviewRuntime::new();
        let host_actor = runtime.alloc_actor();
        Self {
            runtime,
            host_actor,
            ui: InteractivePreviewState::default(),
            input_scratch: Vec::new(),
        }
    }
}

impl<Action, FactTarget> RuntimeBackedPreviewState<Action, FactTarget>
where
    Action: Clone,
    FactTarget: Clone,
{
    pub(crate) fn clear_bindings(&mut self) {
        self.ui.clear_bindings();
    }

    #[must_use]
    pub(crate) fn finalize_render(
        &self,
        root: UiNode,
        ops: Vec<RenderOp>,
    ) -> (RenderRoot, FakeRenderState) {
        self.ui.finalize_render(root, ops)
    }

    pub(crate) fn bind_fact_target(&mut self, node_id: NodeId, target: FactTarget) {
        self.ui.bind_fact_target(node_id, target);
    }

    pub(crate) fn attach_port(
        &mut self,
        ops: &mut Vec<RenderOp>,
        node_id: NodeId,
        source_port: SourcePortId,
        kind: UiEventKind,
        action: Action,
    ) {
        self.ui.attach_port(ops, node_id, source_port, kind, action);
    }

    #[must_use]
    pub(crate) fn element_node(
        &mut self,
        retained_key: RetainedNodeKey,
        tag: &str,
        text: Option<String>,
        children: Vec<UiNode>,
    ) -> UiNode {
        self.ui.element_node(retained_key, tag, text, children)
    }

    #[must_use]
    pub(crate) fn process_ui_events(
        &mut self,
        batch: UiEventBatch,
    ) -> Vec<(Action, UiEventKind, Option<String>)> {
        self.input_scratch.clear();
        let mut seq = 0u32;
        for event in batch.events {
            let Some(port) = self.ui.source_port_for_event_port(event.target) else {
                continue;
            };
            self.input_scratch.push(HostInput::Pulse {
                actor: self.host_actor,
                port,
                value: encode_ui_event(event.kind, event.payload.as_deref()),
                seq: self.runtime.causal_seq(seq),
            });
            seq += 1;
        }
        if self.input_scratch.is_empty() {
            return Vec::new();
        }
        let ui = &self.ui;
        let mut decoded = Vec::new();
        self.runtime
            .dispatch_inputs_batches(self.input_scratch.as_slice(), |messages| {
                decoded.extend(messages.iter().filter_map(|message| match message {
                    Msg::SourcePulse { port, value, .. } => {
                        let action = ui.action_for_port(*port)?;
                        let (kind, payload) = decode_ui_event(value)?;
                        Some((action, kind, payload))
                    }
                    _ => None,
                }));
            });
        decoded
    }

    #[must_use]
    pub(crate) fn process_ui_facts(
        &mut self,
        batch: UiFactBatch,
        fact_cell: impl Fn(&FactTarget) -> MirrorCellId,
        fact_target: impl Fn(MirrorCellId) -> Option<FactTarget>,
    ) -> Vec<(FactTarget, UiFactKind)> {
        self.input_scratch.clear();
        let mut seq = 0u32;
        for fact in batch.facts {
            let Some(target) = self.ui.fact_target_for_node(fact.id) else {
                continue;
            };
            let Some(value) = encode_ui_fact(&fact.kind) else {
                continue;
            };
            self.input_scratch.push(HostInput::Mirror {
                actor: self.host_actor,
                cell: fact_cell(&target),
                value,
                seq: self.runtime.causal_seq(seq),
            });
            seq += 1;
        }
        if self.input_scratch.is_empty() {
            return Vec::new();
        }
        let mut decoded = Vec::new();
        self.runtime
            .dispatch_inputs_batches(self.input_scratch.as_slice(), |messages| {
                decoded.extend(messages.iter().filter_map(|message| match message {
                    Msg::MirrorWrite { cell, value, .. } => {
                        let target = fact_target(*cell)?;
                        let kind = decode_ui_fact(value)?;
                        Some((target, kind))
                    }
                    _ => None,
                }));
            });
        decoded
    }
}
