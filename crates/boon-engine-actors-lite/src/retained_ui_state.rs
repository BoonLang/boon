use crate::ir::{RetainedNodeKey, SourcePortId};
use boon_renderer_zoon::FakeRenderState;
use boon_scene::{
    EventPortId, NodeId, RenderDiffBatch, RenderOp, RenderRoot, UiEventKind, UiNode, UiNodeKind,
};
use std::collections::BTreeMap;

#[derive(Debug, Default)]
pub struct RetainedUiState {
    retained_nodes: BTreeMap<RetainedNodeKey, NodeId>,
    event_ports: BTreeMap<SourcePortId, EventPortId>,
}

impl RetainedUiState {
    pub fn element_node(
        &mut self,
        retained_key: RetainedNodeKey,
        tag: &str,
        text: Option<String>,
        children: Vec<UiNode>,
    ) -> UiNode {
        let id = *self
            .retained_nodes
            .entry(retained_key)
            .or_insert_with(NodeId::new);
        UiNode {
            id,
            kind: UiNodeKind::Element {
                tag: tag.to_string(),
                text,
                event_ports: Vec::new(),
            },
            children,
        }
    }

    pub fn attach_port(
        &mut self,
        ops: &mut Vec<RenderOp>,
        node_id: NodeId,
        source_port: SourcePortId,
        kind: UiEventKind,
    ) -> EventPortId {
        let event_port = *self
            .event_ports
            .entry(source_port)
            .or_insert_with(EventPortId::new);
        ops.push(RenderOp::AttachEventPort {
            id: node_id,
            port: event_port,
            kind,
        });
        event_port
    }

    pub fn finalize_render(
        &self,
        root: UiNode,
        ops: Vec<RenderOp>,
    ) -> (RenderRoot, FakeRenderState) {
        let mut state = FakeRenderState::default();
        state
            .apply_batch(&RenderDiffBatch { ops })
            .expect("retained ui render ops should apply");
        (RenderRoot::UiTree(root), state)
    }

    #[must_use]
    pub fn retained_nodes(&self) -> &BTreeMap<RetainedNodeKey, NodeId> {
        &self.retained_nodes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{FunctionInstanceId, ViewSiteId};

    #[test]
    fn reuses_retained_node_ids_and_event_ports() {
        let mut state = RetainedUiState::default();
        let key = RetainedNodeKey {
            view_site: ViewSiteId(1),
            function_instance: Some(FunctionInstanceId(1)),
            mapped_item_identity: None,
        };
        let first = state.element_node(key, "button", Some("+".to_string()), Vec::new());
        let second = state.element_node(key, "button", Some("+".to_string()), Vec::new());
        assert_eq!(first.id, second.id);

        let mut ops = Vec::new();
        let first_port = state.attach_port(&mut ops, first.id, SourcePortId(1), UiEventKind::Click);
        let second_port =
            state.attach_port(&mut ops, second.id, SourcePortId(1), UiEventKind::Click);
        assert_eq!(first_port, second_port);
    }
}
