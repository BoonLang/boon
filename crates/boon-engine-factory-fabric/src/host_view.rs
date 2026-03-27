use crate::debug::HostCommandDebug;
use boon_scene::{
    EventPortId, NodeId, RenderDiffBatch, RenderNode, RenderOp, RenderRoot, UiEventKind, UiNode,
    UiNodeKind,
};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RenderBatchStats {
    pub patch_count: usize,
    pub retained_node_creations: usize,
    pub retained_node_deletions: usize,
    pub host_commands: Vec<HostCommandDebug>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HostViewTree {
    pub root: Option<HostViewNode>,
}

impl HostViewTree {
    #[must_use]
    pub fn from_root(root: HostViewNode) -> Self {
        Self { root: Some(root) }
    }

    #[allow(dead_code)]
    #[must_use]
    pub fn into_render_batch(&self) -> RenderDiffBatch {
        self.into_render_batch_with_stats().0
    }

    #[must_use]
    pub fn into_render_batch_with_stats(&self) -> (RenderDiffBatch, RenderBatchStats) {
        let Some(root) = &self.root else {
            return (RenderDiffBatch::default(), RenderBatchStats::default());
        };

        let mut ops = vec![RenderOp::ReplaceRoot(RenderRoot::UiTree(root.to_ui_node()))];
        root.append_full_state_ops(&mut ops);
        let stats = RenderBatchStats {
            patch_count: ops.len(),
            retained_node_creations: root.subtree_node_count(),
            retained_node_deletions: 0,
            host_commands: host_commands_for_ops(&ops),
        };
        (RenderDiffBatch { ops }, stats)
    }

    #[allow(dead_code)]
    #[must_use]
    pub fn diff(&self, next: &Self) -> RenderDiffBatch {
        self.diff_with_stats(next).0
    }

    #[must_use]
    pub fn diff_with_stats(&self, next: &Self) -> (RenderDiffBatch, RenderBatchStats) {
        match (&self.root, &next.root) {
            (None, None) => (RenderDiffBatch::default(), RenderBatchStats::default()),
            (None, Some(_)) => next.into_render_batch_with_stats(),
            (Some(current), None) => (
                RenderDiffBatch::default(),
                RenderBatchStats {
                    patch_count: 0,
                    retained_node_creations: 0,
                    retained_node_deletions: current.subtree_node_count(),
                    host_commands: Vec::new(),
                },
            ),
            (Some(current), Some(next_root)) if current.id != next_root.id => {
                let (batch, mut stats) = next.into_render_batch_with_stats();
                stats.retained_node_deletions = current.subtree_node_count();
                (batch, stats)
            }
            (Some(current), Some(next_root)) => {
                let mut ops = Vec::new();
                let mut stats = RenderBatchStats::default();
                diff_node(current, next_root, &mut ops, &mut stats);
                stats.patch_count = ops.len();
                stats.host_commands = host_commands_for_ops(&ops);
                (RenderDiffBatch { ops }, stats)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostViewNode {
    pub id: NodeId,
    pub tag: String,
    pub text: Option<String>,
    pub properties: BTreeMap<String, String>,
    pub styles: BTreeMap<String, String>,
    pub class_flags: BTreeMap<String, bool>,
    pub event_ports: HashMap<EventPortId, UiEventKind>,
    pub input_value: Option<String>,
    pub checked: Option<bool>,
    pub children: Vec<HostViewNode>,
}

#[allow(dead_code)]
impl HostViewNode {
    #[must_use]
    pub fn element(id: NodeId, tag: impl Into<String>) -> Self {
        Self {
            id,
            tag: tag.into(),
            text: None,
            properties: BTreeMap::new(),
            styles: BTreeMap::new(),
            class_flags: BTreeMap::new(),
            event_ports: HashMap::new(),
            input_value: None,
            checked: None,
            children: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    #[must_use]
    pub fn with_property(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.properties.insert(name.into(), value.into());
        self
    }

    #[must_use]
    pub fn with_style(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.styles.insert(name.into(), value.into());
        self
    }

    #[must_use]
    pub fn with_class_flag(mut self, class_name: impl Into<String>, enabled: bool) -> Self {
        self.class_flags.insert(class_name.into(), enabled);
        self
    }

    #[must_use]
    pub fn with_event_port(mut self, port: EventPortId, kind: UiEventKind) -> Self {
        self.event_ports.insert(port, kind);
        self
    }

    #[must_use]
    pub fn with_input_value(mut self, value: impl Into<String>) -> Self {
        self.input_value = Some(value.into());
        self
    }

    #[must_use]
    pub fn with_checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }

    #[must_use]
    pub fn with_children(mut self, children: Vec<HostViewNode>) -> Self {
        self.children = children;
        self
    }

    pub(crate) fn to_ui_node(&self) -> UiNode {
        UiNode {
            id: self.id,
            kind: UiNodeKind::Element {
                tag: self.tag.clone(),
                text: self.text.clone(),
                event_ports: Vec::new(),
            },
            children: self.children.iter().map(Self::to_ui_node).collect(),
        }
    }

    fn subtree_node_count(&self) -> usize {
        1 + self
            .children
            .iter()
            .map(Self::subtree_node_count)
            .sum::<usize>()
    }

    fn append_full_state_ops(&self, ops: &mut Vec<RenderOp>) {
        for (name, value) in &self.properties {
            ops.push(RenderOp::SetProperty {
                id: self.id,
                name: name.clone(),
                value: Some(value.clone()),
            });
        }
        for (name, value) in &self.styles {
            ops.push(RenderOp::SetStyle {
                id: self.id,
                name: name.clone(),
                value: Some(value.clone()),
            });
        }
        for (class_name, enabled) in &self.class_flags {
            ops.push(RenderOp::SetClassFlag {
                id: self.id,
                class_name: class_name.clone(),
                enabled: *enabled,
            });
        }
        for (port, kind) in &self.event_ports {
            ops.push(RenderOp::AttachEventPort {
                id: self.id,
                port: *port,
                kind: kind.clone(),
            });
        }
        if let Some(value) = &self.input_value {
            ops.push(RenderOp::SetInputValue {
                id: self.id,
                value: value.clone(),
            });
        }
        if let Some(checked) = self.checked {
            ops.push(RenderOp::SetChecked {
                id: self.id,
                checked,
            });
        }
        for child in &self.children {
            child.append_full_state_ops(ops);
        }
    }
}

fn diff_node(
    current: &HostViewNode,
    next: &HostViewNode,
    ops: &mut Vec<RenderOp>,
    stats: &mut RenderBatchStats,
) {
    if current.tag != next.tag {
        return;
    }

    if current.text != next.text {
        ops.push(RenderOp::SetText {
            id: next.id,
            text: next.text.clone().unwrap_or_default(),
        });
    }

    diff_string_map(
        next.id,
        &current.properties,
        &next.properties,
        |id, name, value| RenderOp::SetProperty { id, name, value },
        ops,
    );
    diff_string_map(
        next.id,
        &current.styles,
        &next.styles,
        |id, name, value| RenderOp::SetStyle { id, name, value },
        ops,
    );
    diff_bool_map(next.id, &current.class_flags, &next.class_flags, ops);
    diff_event_ports(next.id, &current.event_ports, &next.event_ports, ops);

    if current.input_value != next.input_value {
        ops.push(RenderOp::SetInputValue {
            id: next.id,
            value: next.input_value.clone().unwrap_or_default(),
        });
    }
    if current.checked != next.checked {
        ops.push(RenderOp::SetChecked {
            id: next.id,
            checked: next.checked.unwrap_or(false),
        });
    }

    diff_children(current, next, ops, stats);
}

fn diff_children(
    current: &HostViewNode,
    next: &HostViewNode,
    ops: &mut Vec<RenderOp>,
    stats: &mut RenderBatchStats,
) {
    let current_by_id = current
        .children
        .iter()
        .map(|child| (child.id, child))
        .collect::<HashMap<_, _>>();
    let next_ids = next
        .children
        .iter()
        .map(|child| child.id)
        .collect::<HashSet<_>>();

    for child in &current.children {
        if !next_ids.contains(&child.id) {
            stats.retained_node_deletions += child.subtree_node_count();
            ops.push(RenderOp::RemoveNode { id: child.id });
        }
    }

    for (index, child) in next.children.iter().enumerate() {
        match current_by_id.get(&child.id).copied() {
            Some(existing) => {
                let old_index = current
                    .children
                    .iter()
                    .position(|candidate| candidate.id == child.id)
                    .unwrap_or(index);
                if old_index != index {
                    ops.push(RenderOp::MoveChild {
                        parent: next.id,
                        id: child.id,
                        index,
                    });
                }
                diff_node(existing, child, ops, stats);
            }
            None => {
                stats.retained_node_creations += child.subtree_node_count();
                ops.push(RenderOp::InsertChild {
                    parent: next.id,
                    index,
                    node: RenderNode::Ui(child.to_ui_node()),
                });
                child.append_full_state_ops(ops);
            }
        }
    }
}

fn host_commands_for_ops(ops: &[RenderOp]) -> Vec<HostCommandDebug> {
    ops.iter()
        .map(|op| HostCommandDebug {
            name: match op {
                RenderOp::ReplaceRoot(_) => "ReplaceRoot",
                RenderOp::InsertChild { .. } => "InsertChild",
                RenderOp::MoveChild { .. } => "MoveChild",
                RenderOp::RemoveNode { .. } => "RemoveNode",
                RenderOp::SetText { .. } => "SetText",
                RenderOp::SetProperty { .. } => "SetProperty",
                RenderOp::SetStyle { .. } => "SetStyle",
                RenderOp::SetClassFlag { .. } => "SetClassFlag",
                RenderOp::AttachEventPort { .. } => "AttachEventPort",
                RenderOp::DetachEventPort { .. } => "DetachEventPort",
                RenderOp::SetInputValue { .. } => "SetInputValue",
                RenderOp::SetChecked { .. } => "SetChecked",
                RenderOp::SetSelectedIndex { .. } => "SetSelectedIndex",
                RenderOp::UpdateSceneParam { .. } => "UpdateSceneParam",
            }
            .to_string(),
        })
        .collect()
}

fn diff_string_map(
    id: NodeId,
    current: &BTreeMap<String, String>,
    next: &BTreeMap<String, String>,
    op: impl Fn(NodeId, String, Option<String>) -> RenderOp,
    ops: &mut Vec<RenderOp>,
) {
    let keys = current
        .keys()
        .chain(next.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    for key in keys {
        let current_value = current.get(&key);
        let next_value = next.get(&key);
        if current_value != next_value {
            ops.push(op(id, key, next_value.cloned()));
        }
    }
}

fn diff_bool_map(
    id: NodeId,
    current: &BTreeMap<String, bool>,
    next: &BTreeMap<String, bool>,
    ops: &mut Vec<RenderOp>,
) {
    let keys = current
        .keys()
        .chain(next.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    for key in keys {
        let next_enabled = next.get(&key).copied().unwrap_or(false);
        if current.get(&key).copied().unwrap_or(false) != next_enabled {
            ops.push(RenderOp::SetClassFlag {
                id,
                class_name: key,
                enabled: next_enabled,
            });
        }
    }
}

fn diff_event_ports(
    id: NodeId,
    current: &HashMap<EventPortId, UiEventKind>,
    next: &HashMap<EventPortId, UiEventKind>,
    ops: &mut Vec<RenderOp>,
) {
    for port in current.keys() {
        if !next.contains_key(port) {
            ops.push(RenderOp::DetachEventPort { id, port: *port });
        }
    }
    for (port, kind) in next {
        if current.get(port) != Some(kind) {
            ops.push(RenderOp::AttachEventPort {
                id,
                port: *port,
                kind: kind.clone(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{HostViewNode, HostViewTree};
    use boon_renderer_zoon::FakeRenderState;
    use boon_scene::{EventPortId, NodeId, RenderOp, RenderRoot, UiEventKind};

    fn root_children(state: &FakeRenderState) -> Vec<NodeId> {
        let Some(RenderRoot::UiTree(root)) = state.root() else {
            panic!("expected UI tree root");
        };
        root.children.iter().map(|child| child.id).collect()
    }

    #[test]
    fn initial_batch_includes_element_state_and_event_ports() {
        let root = NodeId::new();
        let input = NodeId::new();
        let checkbox = NodeId::new();
        let change_port = EventPortId::new();
        let click_port = EventPortId::new();
        let tree = HostViewTree::from_root(HostViewNode::element(root, "div").with_children(vec![
                HostViewNode::element(input, "input")
                    .with_property("placeholder", "What needs doing?")
                    .with_style("outline", "1px solid")
                    .with_class_flag("focused", true)
                    .with_event_port(change_port, UiEventKind::Change)
                    .with_input_value("hello"),
                HostViewNode::element(checkbox, "input")
                    .with_property("type", "checkbox")
                    .with_event_port(click_port, UiEventKind::Click)
                    .with_checked(true),
            ]));

        let batch = tree.into_render_batch();
        assert!(batch.ops.iter().any(|op| matches!(
            op,
            RenderOp::SetProperty { id, name, value }
            if *id == input && name == "placeholder" && value.as_deref() == Some("What needs doing?")
        )));
        assert!(batch.ops.iter().any(|op| matches!(
            op,
            RenderOp::SetStyle { id, name, value }
            if *id == input && name == "outline" && value.as_deref() == Some("1px solid")
        )));
        assert!(batch.ops.iter().any(|op| matches!(
            op,
            RenderOp::SetClassFlag { id, class_name, enabled }
            if *id == input && class_name == "focused" && *enabled
        )));
        assert!(batch.ops.iter().any(|op| matches!(
            op,
            RenderOp::SetInputValue { id, value }
            if *id == input && value == "hello"
        )));
        assert!(batch.ops.iter().any(|op| matches!(
            op,
            RenderOp::SetChecked { id, checked }
            if *id == checkbox && *checked
        )));
        assert!(batch.ops.iter().any(|op| matches!(
            op,
            RenderOp::AttachEventPort { id, port, kind }
            if *id == input && *port == change_port && *kind == UiEventKind::Change
        )));
        assert!(batch.ops.iter().any(|op| matches!(
            op,
            RenderOp::AttachEventPort { id, port, kind }
            if *id == checkbox && *port == click_port && *kind == UiEventKind::Click
        )));
    }

    #[test]
    fn diff_moves_inserts_and_removes_children_without_replacing_root() {
        let root = NodeId::new();
        let a = NodeId::new();
        let b = NodeId::new();
        let c = NodeId::new();
        let d = NodeId::new();

        let current =
            HostViewTree::from_root(HostViewNode::element(root, "ul").with_children(vec![
                HostViewNode::element(a, "li").with_text("A"),
                HostViewNode::element(b, "li").with_text("B"),
                HostViewNode::element(c, "li").with_text("C"),
            ]));
        let next = HostViewTree::from_root(HostViewNode::element(root, "ul").with_children(vec![
            HostViewNode::element(c, "li").with_text("C"),
            HostViewNode::element(a, "li").with_text("A"),
            HostViewNode::element(d, "li").with_text("D"),
        ]));

        let mut state = FakeRenderState::default();
        state
            .apply_batch(&current.into_render_batch())
            .expect("initial batch should apply");
        state
            .apply_batch(&current.diff(&next))
            .expect("diff batch should apply");

        assert_eq!(root_children(&state), vec![c, a, d]);
    }

    #[test]
    fn diff_updates_ports_and_text_without_replacing_survivor_nodes() {
        let root = NodeId::new();
        let child = NodeId::new();
        let first_port = EventPortId::new();
        let second_port = EventPortId::new();
        let current =
            HostViewTree::from_root(HostViewNode::element(root, "div").with_children(vec![
                HostViewNode::element(child, "button")
                    .with_text("Old")
                    .with_event_port(first_port, UiEventKind::Click),
            ]));
        let next = HostViewTree::from_root(HostViewNode::element(root, "div").with_children(vec![
                HostViewNode::element(child, "button")
                    .with_text("New")
                    .with_event_port(second_port, UiEventKind::DoubleClick),
            ]));

        let mut state = FakeRenderState::default();
        state
            .apply_batch(&current.into_render_batch())
            .expect("initial batch should apply");
        state
            .apply_batch(&current.diff(&next))
            .expect("diff batch should apply");

        let Some(RenderRoot::UiTree(root)) = state.root() else {
            panic!("expected UI tree root");
        };
        assert_eq!(root.children[0].id, child);
        assert!(matches!(
            &root.children[0].kind,
            boon_scene::UiNodeKind::Element {
                text: Some(text),
                ..
            } if text == "New"
        ));
        assert_eq!(
            state.event_ports_for(child),
            vec![(second_port, UiEventKind::DoubleClick)]
        );
    }
}
