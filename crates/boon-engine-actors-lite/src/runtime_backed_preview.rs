use crate::bridge::{HostInput, HostSnapshot};
use crate::ids::ActorId;
use crate::interactive_preview::{
    InteractivePreviewState, decode_ui_event, decode_ui_fact, encode_ui_event, encode_ui_fact,
};
use crate::ir::{MirrorCellId, RetainedNodeKey, SourcePortId};
use crate::preview_runtime::PreviewRuntime;
use crate::runtime::{ActorKind, Msg};
use boon_renderer_zoon::FakeRenderState;
use boon_scene::{
    NodeId, RenderOp, RenderRoot, UiEventBatch, UiEventKind, UiFactBatch, UiFactKind, UiNode,
};

#[derive(Debug)]
pub struct RuntimeBackedPreviewState<Action, FactTarget> {
    runtime: PreviewRuntime,
    host_actor: ActorId,
    ui: InteractivePreviewState<Action, FactTarget>,
}

impl<Action, FactTarget> Default for RuntimeBackedPreviewState<Action, FactTarget>
where
    Action: Clone,
    FactTarget: Clone,
{
    fn default() -> Self {
        let mut runtime = PreviewRuntime::new();
        let host_actor = runtime.alloc_actor(ActorKind::SourcePort);
        Self {
            runtime,
            host_actor,
            ui: InteractivePreviewState::default(),
        }
    }
}

impl<Action, FactTarget> RuntimeBackedPreviewState<Action, FactTarget>
where
    Action: Clone,
    FactTarget: Clone,
{
    pub fn clear_bindings(&mut self) {
        self.ui.clear_bindings();
    }

    #[must_use]
    pub fn finalize_render(
        &self,
        root: UiNode,
        ops: Vec<RenderOp>,
    ) -> (RenderRoot, FakeRenderState) {
        self.ui.finalize_render(root, ops)
    }

    pub fn bind_fact_target(&mut self, node_id: NodeId, target: FactTarget) {
        self.ui.bind_fact_target(node_id, target);
    }

    pub fn attach_port(
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
    pub fn element_node(
        &mut self,
        retained_key: RetainedNodeKey,
        tag: &str,
        text: Option<String>,
        children: Vec<UiNode>,
    ) -> UiNode {
        self.ui.element_node(retained_key, tag, text, children)
    }

    #[must_use]
    pub fn retained_nodes(&self) -> &std::collections::BTreeMap<RetainedNodeKey, NodeId> {
        self.ui.retained_nodes()
    }

    #[must_use]
    pub fn process_ui_events(
        &mut self,
        batch: UiEventBatch,
    ) -> Vec<(Action, UiEventKind, Option<String>)> {
        let inputs = self
            .ui
            .resolve_ui_events(batch)
            .into_iter()
            .enumerate()
            .map(|(index, (port, _action, kind, payload))| HostInput::Pulse {
                actor: self.host_actor,
                port,
                value: encode_ui_event(kind, payload.as_deref()),
                seq: self.runtime.causal_seq(index as u32),
            })
            .collect::<Vec<_>>();
        if inputs.is_empty() {
            return Vec::new();
        }
        self.runtime
            .dispatch_snapshot(HostSnapshot::new(inputs))
            .into_iter()
            .filter_map(|(_actor_id, message)| match message {
                Msg::SourcePulse { port, value, .. } => {
                    let action = self.ui.action_for_port(port)?;
                    let (kind, payload) = decode_ui_event(&value)?;
                    Some((action, kind, payload))
                }
                _ => None,
            })
            .collect()
    }

    #[must_use]
    pub fn process_ui_facts(
        &mut self,
        batch: UiFactBatch,
        fact_cell: impl Fn(&FactTarget) -> MirrorCellId,
        fact_target: impl Fn(MirrorCellId) -> Option<FactTarget>,
    ) -> Vec<(FactTarget, UiFactKind)> {
        let inputs = self
            .ui
            .resolve_ui_facts(batch)
            .into_iter()
            .enumerate()
            .filter_map(|(index, (target, kind))| {
                encode_ui_fact(&kind).map(|value| HostInput::Mirror {
                    actor: self.host_actor,
                    cell: fact_cell(&target),
                    value,
                    seq: self.runtime.causal_seq(index as u32),
                })
            })
            .collect::<Vec<_>>();
        if inputs.is_empty() {
            return Vec::new();
        }
        self.runtime
            .dispatch_snapshot(HostSnapshot::new(inputs))
            .into_iter()
            .filter_map(|(_actor_id, message)| match message {
                Msg::MirrorWrite { cell, value, .. } => {
                    let target = fact_target(cell)?;
                    let kind = decode_ui_fact(&value)?;
                    Some((target, kind))
                }
                _ => None,
            })
            .collect()
    }
}
