use std::collections::{HashMap, HashSet};

use boon_scene::{
    EventPortId, NodeId, RenderDiffBatch, RenderNode, RenderOp, RenderRoot, UiEventKind, UiNode,
    UiNodeKind,
};

use super::exec_ir::ExecProgram;

pub fn emit_render_batch(program: &ExecProgram) -> RenderDiffBatch {
    let mut ops = Vec::with_capacity(1 + program.setup_ops.len());
    ops.push(RenderOp::ReplaceRoot(program.root.clone()));
    ops.extend(program.setup_ops.clone());
    RenderDiffBatch { ops }
}

pub fn emit_render_diff(previous: &ExecProgram, current: &ExecProgram) -> RenderDiffBatch {
    let (RenderRoot::UiTree(previous_root), RenderRoot::UiTree(current_root)) =
        (&previous.root, &current.root)
    else {
        return emit_render_batch(current);
    };

    if !can_diff_ui_nodes(previous_root, current_root) {
        return emit_render_batch(current);
    }

    let previous_snapshot = RenderSnapshot::from_exec(previous);
    let current_snapshot = RenderSnapshot::from_exec(current);

    let mut ops = Vec::new();
    diff_ui_nodes(previous_root, current_root, None, 0, &mut ops);
    diff_node_state(
        &previous_snapshot,
        &current_snapshot,
        current_root,
        &mut ops,
    );

    RenderDiffBatch { ops }
}

#[derive(Debug, Default)]
struct RenderSnapshot {
    properties: HashMap<NodeId, HashMap<String, Option<String>>>,
    styles: HashMap<NodeId, HashMap<String, Option<String>>>,
    class_flags: HashMap<NodeId, HashMap<String, bool>>,
    event_ports: HashMap<NodeId, HashMap<EventPortId, UiEventKind>>,
    input_values: HashMap<NodeId, String>,
    checked_values: HashMap<NodeId, bool>,
    selected_indices: HashMap<NodeId, Option<usize>>,
}

impl RenderSnapshot {
    fn from_exec(program: &ExecProgram) -> Self {
        let mut snapshot = Self::default();
        for op in &program.setup_ops {
            match op {
                RenderOp::SetProperty { id, name, value } => {
                    snapshot
                        .properties
                        .entry(*id)
                        .or_default()
                        .insert(name.clone(), value.clone());
                }
                RenderOp::SetStyle { id, name, value } => {
                    snapshot
                        .styles
                        .entry(*id)
                        .or_default()
                        .insert(name.clone(), value.clone());
                }
                RenderOp::SetClassFlag {
                    id,
                    class_name,
                    enabled,
                } => {
                    snapshot
                        .class_flags
                        .entry(*id)
                        .or_default()
                        .insert(class_name.clone(), *enabled);
                }
                RenderOp::AttachEventPort { id, port, kind } => {
                    snapshot
                        .event_ports
                        .entry(*id)
                        .or_default()
                        .insert(*port, kind.clone());
                }
                RenderOp::SetInputValue { id, value } => {
                    snapshot.input_values.insert(*id, value.clone());
                }
                RenderOp::SetChecked { id, checked } => {
                    snapshot.checked_values.insert(*id, *checked);
                }
                RenderOp::SetSelectedIndex { id, index } => {
                    snapshot.selected_indices.insert(*id, *index);
                }
                RenderOp::ReplaceRoot(_)
                | RenderOp::InsertChild { .. }
                | RenderOp::RemoveNode { .. }
                | RenderOp::MoveChild { .. }
                | RenderOp::SetText { .. }
                | RenderOp::DetachEventPort { .. }
                | RenderOp::UpdateSceneParam { .. } => {}
            }
        }
        snapshot
    }
}

fn can_diff_ui_nodes(previous: &UiNode, current: &UiNode) -> bool {
    if previous.id != current.id {
        return false;
    }
    match (&previous.kind, &current.kind) {
        (
            UiNodeKind::Element {
                tag: previous_tag, ..
            },
            UiNodeKind::Element {
                tag: current_tag, ..
            },
        ) => previous_tag == current_tag,
        (UiNodeKind::Text { .. }, UiNodeKind::Text { .. }) => true,
        _ => false,
    }
}

fn diff_ui_nodes(
    previous: &UiNode,
    current: &UiNode,
    parent: Option<NodeId>,
    index_in_parent: usize,
    ops: &mut Vec<RenderOp>,
) {
    if !can_diff_ui_nodes(previous, current) {
        if let Some(parent) = parent {
            ops.push(RenderOp::RemoveNode { id: previous.id });
            ops.push(RenderOp::InsertChild {
                parent,
                index: index_in_parent,
                node: RenderNode::Ui(current.clone()),
            });
        } else {
            ops.push(RenderOp::ReplaceRoot(RenderRoot::UiTree(current.clone())));
        }
        return;
    }

    match (&previous.kind, &current.kind) {
        (
            UiNodeKind::Text {
                text: previous_text,
            },
            UiNodeKind::Text { text: current_text },
        ) if previous_text != current_text => {
            ops.push(RenderOp::SetText {
                id: current.id,
                text: current_text.clone(),
            });
        }
        (
            UiNodeKind::Element {
                text: previous_text,
                ..
            },
            UiNodeKind::Element {
                text: current_text, ..
            },
        ) if previous_text != current_text => {
            ops.push(RenderOp::SetText {
                id: current.id,
                text: current_text.clone().unwrap_or_default(),
            });
        }
        _ => {}
    }

    let previous_children_by_id: HashMap<NodeId, (usize, &UiNode)> = previous
        .children
        .iter()
        .enumerate()
        .map(|(index, child)| (child.id, (index, child)))
        .collect();
    let current_ids: HashSet<NodeId> = current.children.iter().map(|child| child.id).collect();

    for child in &previous.children {
        if !current_ids.contains(&child.id) {
            ops.push(RenderOp::RemoveNode { id: child.id });
        }
    }

    for (index, child) in current.children.iter().enumerate() {
        match previous_children_by_id.get(&child.id) {
            Some((previous_index, previous_child)) => {
                if *previous_index != index {
                    ops.push(RenderOp::MoveChild {
                        parent: current.id,
                        id: child.id,
                        index,
                    });
                }
                diff_ui_nodes(previous_child, child, Some(current.id), index, ops);
            }
            None => ops.push(RenderOp::InsertChild {
                parent: current.id,
                index,
                node: RenderNode::Ui(child.clone()),
            }),
        }
    }
}

fn diff_node_state(
    previous: &RenderSnapshot,
    current: &RenderSnapshot,
    node: &UiNode,
    ops: &mut Vec<RenderOp>,
) {
    diff_properties(
        node.id,
        previous.properties.get(&node.id),
        current.properties.get(&node.id),
        ops,
    );
    diff_properties(
        node.id,
        previous.styles.get(&node.id),
        current.styles.get(&node.id),
        ops,
    );
    diff_class_flags(
        node.id,
        previous.class_flags.get(&node.id),
        current.class_flags.get(&node.id),
        ops,
    );
    diff_event_ports(
        node.id,
        previous.event_ports.get(&node.id),
        current.event_ports.get(&node.id),
        ops,
    );
    diff_input_value(
        node.id,
        previous.input_values.get(&node.id),
        current.input_values.get(&node.id),
        ops,
    );
    diff_checked_value(
        node.id,
        previous.checked_values.get(&node.id),
        current.checked_values.get(&node.id),
        ops,
    );
    diff_selected_index(
        node.id,
        previous.selected_indices.get(&node.id),
        current.selected_indices.get(&node.id),
        ops,
    );

    for child in &node.children {
        diff_node_state(previous, current, child, ops);
    }
}

fn diff_properties(
    id: NodeId,
    previous: Option<&HashMap<String, Option<String>>>,
    current: Option<&HashMap<String, Option<String>>>,
    ops: &mut Vec<RenderOp>,
) {
    let mut keys = HashSet::new();
    if let Some(previous) = previous {
        keys.extend(previous.keys().cloned());
    }
    if let Some(current) = current {
        keys.extend(current.keys().cloned());
    }
    for key in keys {
        let previous_value = previous.and_then(|map| map.get(&key));
        let current_value = current.and_then(|map| map.get(&key));
        if previous_value != current_value {
            ops.push(RenderOp::SetProperty {
                id,
                name: key,
                value: current_value.cloned().unwrap_or(None),
            });
        }
    }
}

fn diff_class_flags(
    id: NodeId,
    previous: Option<&HashMap<String, bool>>,
    current: Option<&HashMap<String, bool>>,
    ops: &mut Vec<RenderOp>,
) {
    let mut keys = HashSet::new();
    if let Some(previous) = previous {
        keys.extend(previous.keys().cloned());
    }
    if let Some(current) = current {
        keys.extend(current.keys().cloned());
    }
    for key in keys {
        let previous_enabled = previous
            .and_then(|map| map.get(&key))
            .copied()
            .unwrap_or(false);
        let current_enabled = current
            .and_then(|map| map.get(&key))
            .copied()
            .unwrap_or(false);
        if previous_enabled != current_enabled {
            ops.push(RenderOp::SetClassFlag {
                id,
                class_name: key,
                enabled: current_enabled,
            });
        }
    }
}

fn diff_event_ports(
    id: NodeId,
    previous: Option<&HashMap<EventPortId, UiEventKind>>,
    current: Option<&HashMap<EventPortId, UiEventKind>>,
    ops: &mut Vec<RenderOp>,
) {
    let mut ports = HashSet::new();
    if let Some(previous) = previous {
        ports.extend(previous.keys().copied());
    }
    if let Some(current) = current {
        ports.extend(current.keys().copied());
    }
    for port in ports {
        let previous_kind = previous.and_then(|map| map.get(&port));
        let current_kind = current.and_then(|map| map.get(&port));
        match (previous_kind, current_kind) {
            (Some(previous_kind), Some(current_kind)) if previous_kind != current_kind => {
                ops.push(RenderOp::AttachEventPort {
                    id,
                    port,
                    kind: current_kind.clone(),
                });
            }
            (None, Some(current_kind)) => {
                ops.push(RenderOp::AttachEventPort {
                    id,
                    port,
                    kind: current_kind.clone(),
                });
            }
            (Some(_), None) => ops.push(RenderOp::DetachEventPort { id, port }),
            (None, None) | (Some(_), Some(_)) => {}
        }
    }
}

fn diff_input_value(
    id: NodeId,
    previous: Option<&String>,
    current: Option<&String>,
    ops: &mut Vec<RenderOp>,
) {
    if previous != current {
        if let Some(current) = current {
            ops.push(RenderOp::SetInputValue {
                id,
                value: current.clone(),
            });
        }
    }
}

fn diff_checked_value(
    id: NodeId,
    previous: Option<&bool>,
    current: Option<&bool>,
    ops: &mut Vec<RenderOp>,
) {
    if previous != current {
        if let Some(current) = current {
            ops.push(RenderOp::SetChecked {
                id,
                checked: *current,
            });
        }
    }
}

fn diff_selected_index(
    id: NodeId,
    previous: Option<&Option<usize>>,
    current: Option<&Option<usize>>,
    ops: &mut Vec<RenderOp>,
) {
    if previous != current {
        ops.push(RenderOp::SetSelectedIndex {
            id,
            index: current.copied().unwrap_or(None),
        });
    }
}

#[cfg(test)]
mod tests {
    use boon_scene::{RenderRoot, UiNodeKind};

    use super::{emit_render_batch, emit_render_diff};
    use crate::exec_ir::ExecProgram;
    use crate::semantic_ir::{
        RuntimeModel, ScalarUpdate, SemanticAction, SemanticEventBinding, SemanticNode,
        SemanticProgram,
    };

    #[test]
    fn emit_render_batch_replaces_root_from_exec_program() {
        let exec = ExecProgram::from_semantic(&SemanticProgram {
            root: SemanticNode::element(
                "div",
                Some("root".to_string()),
                vec![("role".to_string(), "status".to_string())],
                Vec::new(),
                Vec::new(),
            ),
            runtime: RuntimeModel::Static,
        });
        let batch = emit_render_batch(&exec);

        assert_eq!(batch.ops.len(), 2);
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(_)) = &batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };
    }

    #[test]
    fn emit_render_diff_uses_stable_ids_for_identical_programs() {
        let semantic = SemanticProgram {
            root: SemanticNode::element(
                "div",
                Some("root".to_string()),
                vec![("role".to_string(), "status".to_string())],
                vec![SemanticEventBinding {
                    kind: boon_scene::UiEventKind::Click,
                    source_binding: Some("counter_button".to_string()),
                    action: Some(SemanticAction::UpdateScalars {
                        updates: vec![ScalarUpdate::Add {
                            binding: "counter".to_string(),
                            delta: 1,
                        }],
                    }),
                }],
                vec![SemanticNode::text("child")],
            ),
            runtime: RuntimeModel::Static,
        };
        let previous = ExecProgram::from_semantic(&semantic);
        let current = ExecProgram::from_semantic(&semantic);

        let RenderRoot::UiTree(previous_root) = &previous.root else {
            panic!("expected ui root");
        };
        let RenderRoot::UiTree(current_root) = &current.root else {
            panic!("expected ui root");
        };
        assert_eq!(previous_root.id, current_root.id);

        let batch = emit_render_diff(&previous, &current);
        assert!(
            batch.ops.is_empty(),
            "identical exec programs should diff to no ops"
        );
    }

    #[test]
    fn emit_render_diff_updates_changed_text_without_replace_root() {
        let previous = ExecProgram::from_semantic(&SemanticProgram {
            root: SemanticNode::element(
                "div",
                None,
                Vec::new(),
                Vec::new(),
                vec![SemanticNode::text("old")],
            ),
            runtime: RuntimeModel::Static,
        });
        let current = ExecProgram::from_semantic(&SemanticProgram {
            root: SemanticNode::element(
                "div",
                None,
                Vec::new(),
                Vec::new(),
                vec![SemanticNode::text("new")],
            ),
            runtime: RuntimeModel::Static,
        });

        let batch = emit_render_diff(&previous, &current);
        assert!(
            batch
                .ops
                .iter()
                .all(|op| !matches!(op, boon_scene::RenderOp::ReplaceRoot(_)))
        );
        assert!(
            batch.ops.iter().any(
                |op| matches!(op, boon_scene::RenderOp::SetText { text, .. } if text == "new")
            )
        );
    }

    #[test]
    fn emit_render_diff_falls_back_when_root_tag_changes() {
        let previous = ExecProgram::from_semantic(&SemanticProgram {
            root: SemanticNode::element("div", None, Vec::new(), Vec::new(), Vec::new()),
            runtime: RuntimeModel::Static,
        });
        let current = ExecProgram::from_semantic(&SemanticProgram {
            root: SemanticNode::element("section", None, Vec::new(), Vec::new(), Vec::new()),
            runtime: RuntimeModel::Static,
        });

        let batch = emit_render_diff(&previous, &current);
        assert!(matches!(
            batch.ops.first(),
            Some(boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(_)))
        ));
    }

    #[test]
    fn exec_program_converts_semantic_tree_to_render_root() {
        let semantic = SemanticProgram {
            root: SemanticNode::element(
                "div",
                Some("root".to_string()),
                vec![("role".to_string(), "status".to_string())],
                vec![SemanticEventBinding {
                    kind: boon_scene::UiEventKind::Click,
                    source_binding: Some("counter_button".to_string()),
                    action: Some(SemanticAction::UpdateScalars {
                        updates: vec![ScalarUpdate::Add {
                            binding: "counter".to_string(),
                            delta: 1,
                        }],
                    }),
                }],
                vec![SemanticNode::text("child")],
            ),
            runtime: RuntimeModel::Static,
        };
        let exec = ExecProgram::from_semantic(&semantic);

        let RenderRoot::UiTree(root) = exec.root else {
            panic!("expected ui render root");
        };
        let UiNodeKind::Element { tag, text, .. } = root.kind else {
            panic!("expected element root");
        };
        assert_eq!(tag, "div");
        assert_eq!(text.as_deref(), Some("root"));
        assert_eq!(root.children.len(), 1);
        assert_eq!(exec.setup_ops.len(), 2);
        assert!(matches!(
            exec.setup_ops[0],
            boon_scene::RenderOp::SetProperty { .. }
        ));
        assert!(matches!(
            exec.setup_ops[1],
            boon_scene::RenderOp::AttachEventPort { .. }
        ));
        assert_eq!(exec.event_bindings.len(), 1);
    }
}
