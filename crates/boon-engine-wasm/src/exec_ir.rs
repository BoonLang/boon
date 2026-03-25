use boon_scene::{EventPortId, NodeId, RenderOp, RenderRoot, UiNode, UiNodeKind};
use ulid::Ulid;

use super::semantic_ir::{
    DerivedScalarOperand, IntCompareOp, ObjectListFilter, ObjectListItem, RuntimeModel,
    SemanticAction, SemanticFactKind, SemanticNode, SemanticProgram, TextListFilter,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecEventBinding {
    pub port: EventPortId,
    pub source_binding: Option<String>,
    pub action: Option<SemanticAction>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecFactBinding {
    pub id: NodeId,
    pub kind: SemanticFactKind,
    pub binding: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecProgram {
    pub root: RenderRoot,
    pub setup_ops: Vec<RenderOp>,
    pub runtime: RuntimeModel,
    pub event_bindings: Vec<ExecEventBinding>,
    pub fact_bindings: Vec<ExecFactBinding>,
    pub semantic_root: SemanticNode,
}

impl ExecProgram {
    #[must_use]
    pub fn from_semantic(program: &SemanticProgram) -> Self {
        let mut setup_ops = Vec::new();
        let mut event_bindings = Vec::new();
        let mut fact_bindings = Vec::new();
        let materialized_root = materialize_initial_bool_branches(&program.root, &program.runtime);
        Self {
            root: RenderRoot::UiTree(semantic_to_ui_root(
                &materialized_root,
                &[],
                &mut setup_ops,
                &mut event_bindings,
                &mut fact_bindings,
            )),
            setup_ops,
            runtime: program.runtime.clone(),
            event_bindings,
            fact_bindings,
            semantic_root: program.root.clone(),
        }
    }
}

fn materialize_initial_bool_branches(node: &SemanticNode, runtime: &RuntimeModel) -> SemanticNode {
    match node {
        SemanticNode::Fragment(children) => SemanticNode::Fragment(
            children
                .iter()
                .map(|child| materialize_initial_bool_branches(child, runtime))
                .collect(),
        ),
        SemanticNode::Keyed { key, node } => SemanticNode::Keyed {
            key: *key,
            node: Box::new(materialize_initial_bool_branches(node, runtime)),
        },
        SemanticNode::Element {
            tag,
            text,
            properties,
            input_value,
            style_fragments,
            event_bindings,
            fact_bindings,
            children,
        } => SemanticNode::Element {
            tag: tag.clone(),
            text: text.clone(),
            properties: properties.clone(),
            input_value: input_value.clone(),
            style_fragments: style_fragments.clone(),
            event_bindings: event_bindings.clone(),
            fact_bindings: fact_bindings.clone(),
            children: children
                .iter()
                .map(|child| materialize_initial_bool_branches(child, runtime))
                .collect(),
        },
        SemanticNode::BoolBranch {
            binding,
            truthy,
            falsy,
        } => {
            let branch = if scalar_runtime_value(runtime, binding) != 0 {
                truthy
            } else {
                falsy
            };
            materialize_initial_bool_branches(branch, runtime)
        }
        SemanticNode::ScalarCompareBranch {
            left,
            op,
            right,
            truthy,
            falsy,
        } => {
            let branch = if scalar_compare_matches(runtime, left, op.clone(), right) {
                truthy
            } else {
                falsy
            };
            materialize_initial_bool_branches(branch, runtime)
        }
        SemanticNode::ObjectScalarCompareBranch { falsy, .. } => {
            materialize_initial_bool_branches(falsy, runtime)
        }
        SemanticNode::ObjectBoolFieldBranch { falsy, .. } => {
            materialize_initial_bool_branches(falsy, runtime)
        }
        SemanticNode::ObjectTextFieldBranch { falsy, .. } => {
            materialize_initial_bool_branches(falsy, runtime)
        }
        SemanticNode::ListEmptyBranch {
            binding,
            object_items,
            invert,
            truthy,
            falsy,
        } => {
            let is_empty = if *object_items {
                object_list_len(runtime, binding) == 0
            } else {
                text_list_len(runtime, binding) == 0
            };
            let branch = if is_empty != *invert { truthy } else { falsy };
            materialize_initial_bool_branches(branch, runtime)
        }
        SemanticNode::Text(text) => SemanticNode::Text(text.clone()),
        SemanticNode::TextTemplate { parts, value } => SemanticNode::TextTemplate {
            parts: parts.clone(),
            value: value.clone(),
        },
        SemanticNode::TextBindingBranch { falsy, .. } => {
            materialize_initial_bool_branches(falsy, runtime)
        }
        SemanticNode::ScalarValue { binding, value } => SemanticNode::ScalarValue {
            binding: binding.clone(),
            value: *value,
        },
        SemanticNode::ObjectFieldValue { field } => SemanticNode::ObjectFieldValue {
            field: field.clone(),
        },
        SemanticNode::TextList {
            binding,
            values,
            filter,
            template,
        } => SemanticNode::TextList {
            binding: binding.clone(),
            values: values.clone(),
            filter: filter.clone(),
            template: template.clone(),
        },
        SemanticNode::ObjectList {
            binding,
            filter,
            item_actions,
            template,
        } => SemanticNode::ObjectList {
            binding: binding.clone(),
            filter: filter.clone(),
            item_actions: item_actions.clone(),
            template: template.clone(),
        },
    }
}

fn scalar_runtime_value(runtime: &RuntimeModel, binding: &str) -> i64 {
    match runtime {
        RuntimeModel::Static => 0,
        RuntimeModel::Scalars(model) => model.values.get(binding).copied().unwrap_or_default(),
        RuntimeModel::State(model) => model
            .scalar_values
            .get(binding)
            .copied()
            .unwrap_or_default(),
    }
}

fn text_list_matches_filter(value: &str, filter: &TextListFilter) -> bool {
    match filter {
        TextListFilter::IntCompare { op, value: target } => {
            let parsed = value.trim().parse::<i64>().unwrap_or_default();
            match op {
                IntCompareOp::Equal => parsed == *target,
                IntCompareOp::NotEqual => parsed != *target,
                IntCompareOp::Greater => parsed > *target,
                IntCompareOp::GreaterOrEqual => parsed >= *target,
                IntCompareOp::Less => parsed < *target,
                IntCompareOp::LessOrEqual => parsed <= *target,
            }
        }
    }
}

fn object_list_item_matches_filter(
    item: &ObjectListItem,
    filter: &ObjectListFilter,
    scalar_values: &std::collections::BTreeMap<String, i64>,
    text_values: &std::collections::BTreeMap<String, String>,
) -> bool {
    match filter {
        ObjectListFilter::BoolFieldEquals { field, value } => match field.as_str() {
            "completed" => item.completed == *value,
            other => item.bool_fields.get(other).copied().unwrap_or(false) == *value,
        },
        ObjectListFilter::SelectedCompletedByScalar { binding } => {
            match scalar_values.get(binding).copied().unwrap_or_default() {
                0 => true,
                1 => !item.completed,
                2 => item.completed,
                _ => true,
            }
        }
        ObjectListFilter::TextFieldStartsWithTextBinding { field, binding } => {
            let actual = match field.as_str() {
                "title" => item.title.as_str(),
                other => item
                    .text_fields
                    .get(other)
                    .map(String::as_str)
                    .unwrap_or_default(),
            };
            actual.starts_with(
                text_values
                    .get(binding)
                    .map(String::as_str)
                    .unwrap_or_default(),
            )
        }
        ObjectListFilter::ItemIdEqualsScalarBinding { binding } => scalar_values
            .get(binding)
            .is_some_and(|value| *value == item.id as i64),
    }
}

fn scalar_operand_value(runtime: &RuntimeModel, operand: &DerivedScalarOperand) -> i64 {
    match operand {
        DerivedScalarOperand::Binding(binding) => scalar_runtime_value(runtime, binding),
        // Initial exec-IR materialization has no text-binding value store.
        // Treat unresolved parsed text as zero until runtime event state exists.
        DerivedScalarOperand::TextBindingNumber(_) => 0,
        DerivedScalarOperand::TextListCount { binding, filter } => match runtime {
            RuntimeModel::State(model) => match filter {
                None => model
                    .text_lists
                    .get(binding)
                    .map_or(0, |items| items.len() as i64),
                Some(filter) => model
                    .text_lists
                    .get(binding)
                    .map(|items| {
                        items
                            .iter()
                            .filter(|value| text_list_matches_filter(value, filter))
                            .count() as i64
                    })
                    .unwrap_or_default(),
            },
            _ => 0,
        },
        DerivedScalarOperand::ObjectListCount { binding, filter } => match runtime {
            RuntimeModel::State(model) => match filter {
                None => model
                    .object_lists
                    .get(binding)
                    .map_or(0, |items| items.len() as i64),
                Some(filter) => model
                    .object_lists
                    .get(binding)
                    .map(|items| {
                        items
                            .iter()
                            .filter(|item| {
                                object_list_item_matches_filter(
                                    item,
                                    filter,
                                    &model.scalar_values,
                                    &model.text_values,
                                )
                            })
                            .count() as i64
                    })
                    .unwrap_or_default(),
            },
            _ => 0,
        },
        DerivedScalarOperand::Literal(value) => *value,
        DerivedScalarOperand::Arithmetic { op, left, right } => {
            let left = scalar_operand_value(runtime, left);
            let right = scalar_operand_value(runtime, right);
            match op {
                crate::semantic_ir::DerivedArithmeticOp::Add => left + right,
                crate::semantic_ir::DerivedArithmeticOp::Subtract => left - right,
                crate::semantic_ir::DerivedArithmeticOp::Multiply => left * right,
                crate::semantic_ir::DerivedArithmeticOp::Divide => {
                    if right == 0 {
                        0
                    } else {
                        left / right
                    }
                }
            }
        }
        DerivedScalarOperand::Min { left, right } => {
            scalar_operand_value(runtime, left).min(scalar_operand_value(runtime, right))
        }
        DerivedScalarOperand::Round { source } => scalar_operand_value(runtime, source),
    }
}

fn scalar_compare_matches(
    runtime: &RuntimeModel,
    left: &DerivedScalarOperand,
    op: IntCompareOp,
    right: &DerivedScalarOperand,
) -> bool {
    let left = scalar_operand_value(runtime, left);
    let right = scalar_operand_value(runtime, right);
    match op {
        IntCompareOp::Equal => left == right,
        IntCompareOp::NotEqual => left != right,
        IntCompareOp::Greater => left > right,
        IntCompareOp::GreaterOrEqual => left >= right,
        IntCompareOp::Less => left < right,
        IntCompareOp::LessOrEqual => left <= right,
    }
}

fn semantic_to_ui_root(
    node: &SemanticNode,
    path: &[usize],
    setup_ops: &mut Vec<RenderOp>,
    event_bindings: &mut Vec<ExecEventBinding>,
    fact_bindings: &mut Vec<ExecFactBinding>,
) -> UiNode {
    match node {
        SemanticNode::Fragment(children) => {
            ui_element_node(path, "div".to_string(), None, Vec::new()).with_children(
                semantic_children_to_ui_nodes(
                    children,
                    path,
                    setup_ops,
                    event_bindings,
                    fact_bindings,
                ),
            )
        }
        _ => semantic_to_ui_node(node, path, setup_ops, event_bindings, fact_bindings),
    }
}

fn semantic_children_to_ui_nodes(
    nodes: &[SemanticNode],
    parent_path: &[usize],
    setup_ops: &mut Vec<RenderOp>,
    event_bindings: &mut Vec<ExecEventBinding>,
    fact_bindings: &mut Vec<ExecFactBinding>,
) -> Vec<UiNode> {
    let mut output = Vec::new();
    for (index, node) in nodes.iter().enumerate() {
        match node {
            SemanticNode::Fragment(children) => {
                let path_for_child = child_path(parent_path, index);
                output.extend(semantic_children_to_ui_nodes(
                    children,
                    &path_for_child,
                    setup_ops,
                    event_bindings,
                    fact_bindings,
                ));
            }
            SemanticNode::Keyed { key, node } => {
                let path_for_child = keyed_child_path(parent_path, *key);
                output.push(semantic_to_ui_node(
                    node,
                    &path_for_child,
                    setup_ops,
                    event_bindings,
                    fact_bindings,
                ));
            }
            SemanticNode::TextList {
                values, template, ..
            } => {
                let path_for_child = child_path(parent_path, index);
                output.extend(values.iter().enumerate().map(|(list_index, value)| {
                    let item_path = child_path(&path_for_child, list_index);
                    let child = ui_element_node(&item_path, template.tag.clone(), None, Vec::new())
                        .with_children(vec![ui_text_node(
                            &child_path(&item_path, 0),
                            format!("{}{}{}", template.prefix, value, template.suffix),
                        )]);
                    for (name, property_value) in &template.properties {
                        setup_ops.push(RenderOp::SetProperty {
                            id: child.id,
                            name: name.clone(),
                            value: Some(property_value.clone()),
                        });
                    }
                    child
                }));
            }
            _ => {
                let path_for_child = child_path(parent_path, index);
                output.push(semantic_to_ui_node(
                    node,
                    &path_for_child,
                    setup_ops,
                    event_bindings,
                    fact_bindings,
                ))
            }
        }
    }
    output
}

fn semantic_to_ui_node(
    node: &SemanticNode,
    path: &[usize],
    setup_ops: &mut Vec<RenderOp>,
    exec_event_bindings: &mut Vec<ExecEventBinding>,
    exec_fact_bindings: &mut Vec<ExecFactBinding>,
) -> UiNode {
    match node {
        SemanticNode::Fragment(children) => {
            ui_element_node(path, "div".to_string(), None, Vec::new()).with_children(
                semantic_children_to_ui_nodes(
                    children,
                    path,
                    setup_ops,
                    exec_event_bindings,
                    exec_fact_bindings,
                ),
            )
        }
        SemanticNode::Keyed { node, .. } => semantic_to_ui_node(
            node,
            path,
            setup_ops,
            exec_event_bindings,
            exec_fact_bindings,
        ),
        SemanticNode::Element {
            tag,
            text,
            properties,
            input_value: _,
            style_fragments: _,
            event_bindings,
            fact_bindings,
            children,
        } => {
            let event_ports: Vec<EventPortId> = event_bindings
                .iter()
                .enumerate()
                .map(|(index, binding)| stable_event_port_id(path, index, &binding.kind))
                .collect();
            let node = ui_element_node(path, tag.clone(), text.clone(), event_ports.clone());
            let node_id = node.id;
            for (name, value) in properties {
                setup_ops.push(RenderOp::SetProperty {
                    id: node_id,
                    name: name.clone(),
                    value: Some(value.clone()),
                });
            }
            for (binding, port) in event_bindings.iter().zip(event_ports) {
                setup_ops.push(RenderOp::AttachEventPort {
                    id: node_id,
                    port,
                    kind: binding.kind.clone(),
                });
                exec_event_bindings.push(ExecEventBinding {
                    port,
                    source_binding: binding.source_binding.clone(),
                    action: binding.action.clone(),
                });
            }
            for binding in fact_bindings {
                exec_fact_bindings.push(ExecFactBinding {
                    id: node_id,
                    kind: binding.kind.clone(),
                    binding: binding.binding.clone(),
                });
            }
            let children = semantic_children_to_ui_nodes(
                children,
                path,
                setup_ops,
                exec_event_bindings,
                exec_fact_bindings,
            );
            node.with_children(children)
        }
        SemanticNode::Text(text) => ui_text_node(path, text.clone()),
        SemanticNode::TextTemplate { value, .. } => ui_text_node(path, value.clone()),
        SemanticNode::TextBindingBranch { falsy, .. } => semantic_to_ui_node(
            falsy,
            path,
            setup_ops,
            exec_event_bindings,
            exec_fact_bindings,
        ),
        SemanticNode::BoolBranch { falsy, .. } => semantic_to_ui_node(
            falsy,
            path,
            setup_ops,
            exec_event_bindings,
            exec_fact_bindings,
        ),
        SemanticNode::ScalarCompareBranch { falsy, .. } => semantic_to_ui_node(
            falsy,
            path,
            setup_ops,
            exec_event_bindings,
            exec_fact_bindings,
        ),
        SemanticNode::ObjectScalarCompareBranch { falsy, .. } => semantic_to_ui_node(
            falsy,
            path,
            setup_ops,
            exec_event_bindings,
            exec_fact_bindings,
        ),
        SemanticNode::ObjectBoolFieldBranch { falsy, .. } => semantic_to_ui_node(
            falsy,
            path,
            setup_ops,
            exec_event_bindings,
            exec_fact_bindings,
        ),
        SemanticNode::ObjectTextFieldBranch { falsy, .. } => semantic_to_ui_node(
            falsy,
            path,
            setup_ops,
            exec_event_bindings,
            exec_fact_bindings,
        ),
        SemanticNode::ListEmptyBranch { falsy, .. } => semantic_to_ui_node(
            falsy,
            path,
            setup_ops,
            exec_event_bindings,
            exec_fact_bindings,
        ),
        SemanticNode::ScalarValue { value, .. } => ui_text_node(path, value.to_string()),
        SemanticNode::ObjectFieldValue { .. } => ui_text_node(path, String::new()),
        SemanticNode::TextList {
            values, template, ..
        } => ui_element_node(path, "div".to_string(), None, Vec::new()).with_children(
            values
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    let item_path = child_path(path, index);
                    let child = ui_element_node(&item_path, template.tag.clone(), None, Vec::new())
                        .with_children(vec![ui_text_node(
                            &child_path(&item_path, 0),
                            format!("{}{}{}", template.prefix, value, template.suffix),
                        )]);
                    for (name, property_value) in &template.properties {
                        setup_ops.push(RenderOp::SetProperty {
                            id: child.id,
                            name: name.clone(),
                            value: Some(property_value.clone()),
                        });
                    }
                    child
                })
                .collect(),
        ),
        SemanticNode::ObjectList { .. } => {
            ui_element_node(path, "div".to_string(), None, Vec::new())
        }
    }
}

fn child_path(path: &[usize], index: usize) -> Vec<usize> {
    let mut child_path = path.to_vec();
    child_path.push(index);
    child_path
}

fn keyed_child_path(path: &[usize], key: u64) -> Vec<usize> {
    let mut keyed_path = path.to_vec();
    keyed_path.push(usize::MAX);
    keyed_path.push((key >> 32) as usize);
    keyed_path.push((key & 0xFFFF_FFFF) as usize);
    keyed_path
}

fn ui_element_node(
    path: &[usize],
    tag: String,
    text: Option<String>,
    event_ports: Vec<EventPortId>,
) -> UiNode {
    UiNode {
        id: stable_node_id(path),
        kind: UiNodeKind::Element {
            tag,
            text,
            event_ports,
        },
        children: Vec::new(),
    }
}

fn ui_text_node(path: &[usize], text: String) -> UiNode {
    UiNode {
        id: stable_node_id(path),
        kind: UiNodeKind::Text { text },
        children: Vec::new(),
    }
}

fn stable_node_id(path: &[usize]) -> NodeId {
    NodeId(Ulid::from_bytes(stable_ulid_bytes(b"node", path, 0, None)))
}

fn stable_event_port_id(
    path: &[usize],
    index: usize,
    kind: &boon_scene::UiEventKind,
) -> EventPortId {
    EventPortId(Ulid::from_bytes(stable_ulid_bytes(
        b"port",
        path,
        index as u64,
        Some(kind_tag(kind)),
    )))
}

fn stable_ulid_bytes(namespace: &[u8], path: &[usize], extra: u64, tag: Option<&str>) -> [u8; 16] {
    let left = stable_hash64(namespace, path, extra, tag);
    let right = stable_hash64(
        b"boon-wasm-pro-v2",
        path,
        extra ^ 0x9E37_79B9_7F4A_7C15,
        tag,
    );
    let mut bytes = [0_u8; 16];
    bytes[..8].copy_from_slice(&left.to_be_bytes());
    bytes[8..].copy_from_slice(&right.to_be_bytes());
    bytes
}

fn stable_hash64(namespace: &[u8], path: &[usize], extra: u64, tag: Option<&str>) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    fn write(hash: &mut u64, bytes: &[u8]) {
        for byte in bytes {
            *hash ^= u64::from(*byte);
            *hash = hash.wrapping_mul(PRIME);
        }
    }

    let mut hash = OFFSET;
    write(&mut hash, namespace);
    for segment in path {
        write(&mut hash, &(*segment as u64).to_be_bytes());
    }
    write(&mut hash, &extra.to_be_bytes());
    if let Some(tag) = tag {
        write(&mut hash, tag.as_bytes());
    }
    hash
}

fn kind_tag(kind: &boon_scene::UiEventKind) -> &str {
    match kind {
        boon_scene::UiEventKind::Click => "Click",
        boon_scene::UiEventKind::DoubleClick => "DoubleClick",
        boon_scene::UiEventKind::Input => "Input",
        boon_scene::UiEventKind::Change => "Change",
        boon_scene::UiEventKind::KeyDown => "KeyDown",
        boon_scene::UiEventKind::Blur => "Blur",
        boon_scene::UiEventKind::Focus => "Focus",
        boon_scene::UiEventKind::Custom(name) => name.as_str(),
    }
}

fn text_list_len(runtime: &RuntimeModel, binding: &str) -> usize {
    match runtime {
        RuntimeModel::State(model) => model.text_lists.get(binding).map_or(0, Vec::len),
        _ => 0,
    }
}

fn object_list_len(runtime: &RuntimeModel, binding: &str) -> usize {
    match runtime {
        RuntimeModel::State(model) => model.object_lists.get(binding).map_or(0, Vec::len),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use boon_scene::{RenderOp, RenderRoot, UiNodeKind};

    use super::ExecProgram;
    use crate::semantic_ir::{
        RuntimeModel, ScalarUpdate, SemanticAction, SemanticEventBinding, SemanticNode,
        SemanticProgram,
    };

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
        assert!(matches!(exec.setup_ops[0], RenderOp::SetProperty { .. }));
        assert!(matches!(
            exec.setup_ops[1],
            RenderOp::AttachEventPort { .. }
        ));
        assert_eq!(exec.event_bindings.len(), 1);
    }
}
