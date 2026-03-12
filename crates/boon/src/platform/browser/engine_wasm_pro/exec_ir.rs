use boon_scene::{EventPortId, NodeId, RenderOp, RenderRoot, UiNode, UiNodeKind};

use super::semantic_ir::{
    DerivedScalarOperand, IntCompareOp, RuntimeModel, SemanticAction, SemanticFactKind,
    SemanticNode, SemanticProgram,
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

fn scalar_operand_value(runtime: &RuntimeModel, operand: &DerivedScalarOperand) -> i64 {
    match operand {
        DerivedScalarOperand::Binding(binding) => scalar_runtime_value(runtime, binding),
        DerivedScalarOperand::Literal(value) => *value,
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
    setup_ops: &mut Vec<RenderOp>,
    event_bindings: &mut Vec<ExecEventBinding>,
    fact_bindings: &mut Vec<ExecFactBinding>,
) -> UiNode {
    match node {
        SemanticNode::Fragment(children) => UiNode::new(UiNodeKind::Element {
            tag: "div".to_string(),
            text: None,
            event_ports: Vec::new(),
        })
        .with_children(semantic_children_to_ui_nodes(
            children,
            setup_ops,
            event_bindings,
            fact_bindings,
        )),
        _ => semantic_to_ui_node(node, setup_ops, event_bindings, fact_bindings),
    }
}

fn semantic_children_to_ui_nodes(
    nodes: &[SemanticNode],
    setup_ops: &mut Vec<RenderOp>,
    event_bindings: &mut Vec<ExecEventBinding>,
    fact_bindings: &mut Vec<ExecFactBinding>,
) -> Vec<UiNode> {
    let mut output = Vec::new();
    for node in nodes {
        match node {
            SemanticNode::Fragment(children) => {
                output.extend(semantic_children_to_ui_nodes(
                    children,
                    setup_ops,
                    event_bindings,
                    fact_bindings,
                ));
            }
            SemanticNode::TextList {
                values, template, ..
            } => {
                output.extend(values.iter().map(|value| {
                    let child = UiNode::new(UiNodeKind::Element {
                        tag: template.tag.clone(),
                        text: None,
                        event_ports: Vec::new(),
                    })
                    .with_children(vec![UiNode::new(UiNodeKind::Text {
                        text: format!("{}{}{}", template.prefix, value, template.suffix),
                    })]);
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
            _ => output.push(semantic_to_ui_node(
                node,
                setup_ops,
                event_bindings,
                fact_bindings,
            )),
        }
    }
    output
}

fn semantic_to_ui_node(
    node: &SemanticNode,
    setup_ops: &mut Vec<RenderOp>,
    exec_event_bindings: &mut Vec<ExecEventBinding>,
    exec_fact_bindings: &mut Vec<ExecFactBinding>,
) -> UiNode {
    match node {
        SemanticNode::Fragment(children) => UiNode::new(UiNodeKind::Element {
            tag: "div".to_string(),
            text: None,
            event_ports: Vec::new(),
        })
        .with_children(semantic_children_to_ui_nodes(
            children,
            setup_ops,
            exec_event_bindings,
            exec_fact_bindings,
        )),
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
            let event_ports: Vec<EventPortId> =
                event_bindings.iter().map(|_| EventPortId::new()).collect();
            let node = UiNode::new(UiNodeKind::Element {
                tag: tag.clone(),
                text: text.clone(),
                event_ports: event_ports.clone(),
            });
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
                setup_ops,
                exec_event_bindings,
                exec_fact_bindings,
            );
            node.with_children(children)
        }
        SemanticNode::Text(text) => UiNode::new(UiNodeKind::Text { text: text.clone() }),
        SemanticNode::TextTemplate { value, .. } => UiNode::new(UiNodeKind::Text {
            text: value.clone(),
        }),
        SemanticNode::TextBindingBranch { falsy, .. } => {
            semantic_to_ui_node(falsy, setup_ops, exec_event_bindings, exec_fact_bindings)
        }
        SemanticNode::BoolBranch { falsy, .. } => {
            semantic_to_ui_node(falsy, setup_ops, exec_event_bindings, exec_fact_bindings)
        }
        SemanticNode::ScalarCompareBranch { falsy, .. } => {
            semantic_to_ui_node(falsy, setup_ops, exec_event_bindings, exec_fact_bindings)
        }
        SemanticNode::ObjectScalarCompareBranch { falsy, .. } => {
            semantic_to_ui_node(falsy, setup_ops, exec_event_bindings, exec_fact_bindings)
        }
        SemanticNode::ObjectBoolFieldBranch { falsy, .. } => {
            semantic_to_ui_node(falsy, setup_ops, exec_event_bindings, exec_fact_bindings)
        }
        SemanticNode::ObjectTextFieldBranch { falsy, .. } => {
            semantic_to_ui_node(falsy, setup_ops, exec_event_bindings, exec_fact_bindings)
        }
        SemanticNode::ListEmptyBranch { falsy, .. } => {
            semantic_to_ui_node(falsy, setup_ops, exec_event_bindings, exec_fact_bindings)
        }
        SemanticNode::ScalarValue { value, .. } => UiNode::new(UiNodeKind::Text {
            text: value.to_string(),
        }),
        SemanticNode::ObjectFieldValue { .. } => UiNode::new(UiNodeKind::Text {
            text: String::new(),
        }),
        SemanticNode::TextList {
            values, template, ..
        } => UiNode::new(UiNodeKind::Element {
            tag: "div".to_string(),
            text: None,
            event_ports: Vec::new(),
        })
        .with_children(
            values
                .iter()
                .map(|value| {
                    let child = UiNode::new(UiNodeKind::Element {
                        tag: template.tag.clone(),
                        text: None,
                        event_ports: Vec::new(),
                    })
                    .with_children(vec![UiNode::new(UiNodeKind::Text {
                        text: format!("{}{}{}", template.prefix, value, template.suffix),
                    })]);
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
        SemanticNode::ObjectList { .. } => UiNode::new(UiNodeKind::Element {
            tag: "div".to_string(),
            text: None,
            event_ports: Vec::new(),
        }),
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
    use crate::platform::browser::engine_wasm_pro::semantic_ir::{
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
