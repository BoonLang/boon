use crate::bridge::{HostViewIr, HostViewKind, HostViewNode};
use crate::ir::{FunctionInstanceId, RetainedNodeKey, SinkPortId, SourcePortId, ViewSiteId};
use boon::platform::browser::kernel::KernelValue;
use boon_scene::UiEventKind;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum HostViewTemplate {
    Node(HostViewTemplateNode),
    Conditional {
        condition: HostViewTemplateCondition,
        when_true: Vec<HostViewTemplate>,
        when_false: Vec<HostViewTemplate>,
    },
    Repeat {
        list_sink: SinkPortId,
        item_identity_field: &'static str,
        body: Vec<HostViewTemplate>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct HostViewTemplateNode {
    pub view_site: ViewSiteId,
    pub function_instance: FunctionInstanceId,
    pub kind: HostViewTemplateNodeKind,
    pub children: Vec<HostViewTemplate>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HostViewTemplateNodeKind {
    Concrete(HostViewKind),
    BoundLabel {
        value: HostViewTemplateValue,
    },
    BoundActionLabel {
        value: HostViewTemplateValue,
        press_port: SourcePortId,
        event_kind: UiEventKind,
    },
    BoundCheckbox {
        checked: HostViewTemplateValue,
        click_port: SourcePortId,
        labelled_by_view_site: Option<ViewSiteId>,
    },
    BoundTextInput {
        value: HostViewTemplateValue,
        placeholder: String,
        change_port: SourcePortId,
        key_down_port: SourcePortId,
        blur_port: Option<SourcePortId>,
        focus_port: Option<SourcePortId>,
        focus_on_mount: HostViewTemplateCondition,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum HostViewTemplateValue {
    Static(KernelValue),
    Sink(SinkPortId),
    ItemField(&'static str),
}

#[derive(Debug, Clone, PartialEq)]
pub enum HostViewTemplateCondition {
    Always,
    ListNotEmpty(SinkPortId),
    SinkTruthy(SinkPortId),
    SinkFalsey(SinkPortId),
    SinkGreaterThanZero(SinkPortId),
    SinkTextEquals(SinkPortId, &'static str),
    ItemIdentityEqualsSink(SinkPortId),
}

#[derive(Debug, Clone, Copy)]
struct HostTemplateItem<'a> {
    identity: u64,
    fields: &'a BTreeMap<String, KernelValue>,
}

pub fn materialize_host_view_template(
    template: &HostViewTemplate,
    sink_values: &BTreeMap<SinkPortId, KernelValue>,
) -> Result<HostViewIr, String> {
    let mut roots = materialize_templates(std::slice::from_ref(template), sink_values, None)?;
    let root = if roots.len() == 1 {
        roots.pop()
    } else if roots.is_empty() {
        None
    } else {
        return Err("host-view template materialized more than one root node".to_string());
    };
    Ok(HostViewIr { root })
}

fn materialize_templates(
    templates: &[HostViewTemplate],
    sink_values: &BTreeMap<SinkPortId, KernelValue>,
    current_item: Option<HostTemplateItem<'_>>,
) -> Result<Vec<HostViewNode>, String> {
    let mut nodes = Vec::new();
    for template in templates {
        match template {
            HostViewTemplate::Node(node) => {
                let children = materialize_templates(&node.children, sink_values, current_item)?;
                let kind = materialize_kind(&node.kind, sink_values, current_item)?;
                nodes.push(HostViewNode {
                    retained_key: RetainedNodeKey {
                        view_site: node.view_site,
                        function_instance: Some(node.function_instance),
                        mapped_item_identity: current_item.map(|item| item.identity),
                    },
                    kind,
                    children,
                });
            }
            HostViewTemplate::Conditional {
                condition,
                when_true,
                when_false,
            } => {
                let selected = if evaluate_condition(condition, sink_values, current_item)? {
                    when_true
                } else {
                    when_false
                };
                nodes.extend(materialize_templates(selected, sink_values, current_item)?);
            }
            HostViewTemplate::Repeat {
                list_sink,
                item_identity_field,
                body,
            } => {
                for item in list_binding_items(sink_values, *list_sink, item_identity_field)? {
                    nodes.extend(materialize_templates(body, sink_values, Some(item))?);
                }
            }
        }
    }
    Ok(nodes)
}

fn materialize_kind(
    kind: &HostViewTemplateNodeKind,
    sink_values: &BTreeMap<SinkPortId, KernelValue>,
    current_item: Option<HostTemplateItem<'_>>,
) -> Result<HostViewKind, String> {
    Ok(match kind {
        HostViewTemplateNodeKind::Concrete(kind) => kind.clone(),
        HostViewTemplateNodeKind::BoundLabel { value } => HostViewKind::StaticLabel {
            text: render_template_value(value, sink_values, current_item)?,
        },
        HostViewTemplateNodeKind::BoundActionLabel {
            value,
            press_port,
            event_kind,
        } => HostViewKind::StaticActionLabel {
            text: render_template_value(value, sink_values, current_item)?,
            press_port: *press_port,
            event_kind: event_kind.clone(),
        },
        HostViewTemplateNodeKind::BoundCheckbox {
            checked,
            click_port,
            labelled_by_view_site,
        } => HostViewKind::StaticCheckbox {
            checked: resolve_bool_value(checked, sink_values, current_item)?,
            click_port: *click_port,
            labelled_by_view_site: *labelled_by_view_site,
        },
        HostViewTemplateNodeKind::BoundTextInput {
            value,
            placeholder,
            change_port,
            key_down_port,
            blur_port,
            focus_port,
            focus_on_mount,
        } => HostViewKind::StaticTextInput {
            value: render_template_value(value, sink_values, current_item)?,
            placeholder: placeholder.clone(),
            change_port: *change_port,
            key_down_port: *key_down_port,
            blur_port: *blur_port,
            focus_port: *focus_port,
            focus_on_mount: evaluate_condition(focus_on_mount, sink_values, current_item)?,
            disabled: false,
        },
    })
}

fn list_binding_items<'a>(
    sink_values: &'a BTreeMap<SinkPortId, KernelValue>,
    list_sink: SinkPortId,
    item_identity_field: &str,
) -> Result<Vec<HostTemplateItem<'a>>, String> {
    let Some(KernelValue::List(items)) = sink_values.get(&list_sink) else {
        return Ok(Vec::new());
    };
    let mut resolved = Vec::new();
    for item in items {
        let KernelValue::Object(fields) = item else {
            continue;
        };
        let Some(KernelValue::Number(number)) = fields.get(item_identity_field) else {
            continue;
        };
        if *number < 0.0 {
            continue;
        }
        resolved.push(HostTemplateItem {
            identity: *number as u64,
            fields,
        });
    }
    Ok(resolved)
}

fn render_template_value(
    value: &HostViewTemplateValue,
    sink_values: &BTreeMap<SinkPortId, KernelValue>,
    current_item: Option<HostTemplateItem<'_>>,
) -> Result<String, String> {
    let value = resolve_value(value, sink_values, current_item)?;
    Ok(render_kernel_value(value))
}

fn resolve_bool_value(
    value: &HostViewTemplateValue,
    sink_values: &BTreeMap<SinkPortId, KernelValue>,
    current_item: Option<HostTemplateItem<'_>>,
) -> Result<bool, String> {
    let value = resolve_value(value, sink_values, current_item)?;
    Ok(kernel_value_truthy(value))
}

fn resolve_value<'a>(
    value: &'a HostViewTemplateValue,
    sink_values: &'a BTreeMap<SinkPortId, KernelValue>,
    current_item: Option<HostTemplateItem<'a>>,
) -> Result<&'a KernelValue, String> {
    match value {
        HostViewTemplateValue::Static(value) => Ok(value),
        HostViewTemplateValue::Sink(sink) => sink_values
            .get(sink)
            .ok_or_else(|| format!("missing host-view sink binding for {:?}", sink)),
        HostViewTemplateValue::ItemField(field) => current_item
            .and_then(|item| item.fields.get(*field))
            .ok_or_else(|| format!("missing host-view mapped item field `{field}`")),
    }
}

fn evaluate_condition(
    condition: &HostViewTemplateCondition,
    sink_values: &BTreeMap<SinkPortId, KernelValue>,
    current_item: Option<HostTemplateItem<'_>>,
) -> Result<bool, String> {
    Ok(match condition {
        HostViewTemplateCondition::Always => true,
        HostViewTemplateCondition::ListNotEmpty(sink) => match sink_values.get(sink) {
            Some(KernelValue::List(items)) => !items.is_empty(),
            _ => false,
        },
        HostViewTemplateCondition::SinkTruthy(sink) => {
            sink_values.get(sink).is_some_and(kernel_value_truthy)
        }
        HostViewTemplateCondition::SinkFalsey(sink) => {
            !sink_values.get(sink).is_some_and(kernel_value_truthy)
        }
        HostViewTemplateCondition::SinkGreaterThanZero(sink) => match sink_values.get(sink) {
            Some(KernelValue::Number(number)) => *number > 0.0,
            _ => false,
        },
        HostViewTemplateCondition::SinkTextEquals(sink, expected) => match sink_values.get(sink) {
            Some(KernelValue::Text(text)) | Some(KernelValue::Tag(text)) => text == expected,
            _ => false,
        },
        HostViewTemplateCondition::ItemIdentityEqualsSink(sink) => {
            match (current_item, sink_values.get(sink)) {
                (Some(item), Some(KernelValue::Number(number))) if *number >= 0.0 => {
                    item.identity == *number as u64
                }
                _ => false,
            }
        }
    })
}

pub(crate) fn kernel_value_truthy(value: &KernelValue) -> bool {
    match value {
        KernelValue::Bool(value) => *value,
        KernelValue::Number(value) => *value != 0.0,
        KernelValue::Text(text) | KernelValue::Tag(text) => matches!(text.as_str(), "true" | "1"),
        KernelValue::Skip => false,
        KernelValue::Object(_) | KernelValue::List(_) => true,
    }
}

fn render_kernel_value(value: &KernelValue) -> String {
    match value {
        KernelValue::Number(number) if number.fract() == 0.0 => format!("{}", *number as i64),
        KernelValue::Number(number) => number.to_string(),
        KernelValue::Text(text) | KernelValue::Tag(text) => text.clone(),
        KernelValue::Bool(value) => value.to_string(),
        KernelValue::Skip => String::new(),
        KernelValue::Object(_) | KernelValue::List(_) => format!("{value:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::HostStripeDirection;

    #[test]
    fn materializes_repeated_and_conditional_nodes_with_mapped_identity() {
        let template = HostViewTemplate::Node(HostViewTemplateNode {
            view_site: ViewSiteId(1),
            function_instance: FunctionInstanceId(1),
            kind: HostViewTemplateNodeKind::Concrete(HostViewKind::Document),
            children: vec![HostViewTemplate::Node(HostViewTemplateNode {
                view_site: ViewSiteId(2),
                function_instance: FunctionInstanceId(1),
                kind: HostViewTemplateNodeKind::Concrete(HostViewKind::StripeLayout {
                    direction: HostStripeDirection::Column,
                    gap_px: 0,
                    padding_px: None,
                    width: None,
                    align_cross: None,
                }),
                children: vec![HostViewTemplate::Repeat {
                    list_sink: SinkPortId(1),
                    item_identity_field: "id",
                    body: vec![HostViewTemplate::Conditional {
                        condition: HostViewTemplateCondition::ItemIdentityEqualsSink(SinkPortId(2)),
                        when_true: vec![HostViewTemplate::Node(HostViewTemplateNode {
                            view_site: ViewSiteId(3),
                            function_instance: FunctionInstanceId(2),
                            kind: HostViewTemplateNodeKind::BoundLabel {
                                value: HostViewTemplateValue::ItemField("title"),
                            },
                            children: Vec::new(),
                        })],
                        when_false: Vec::new(),
                    }],
                }],
            })],
        });
        let sink_values = BTreeMap::from([
            (
                SinkPortId(1),
                KernelValue::List(vec![
                    KernelValue::Object(BTreeMap::from([
                        ("id".to_string(), KernelValue::from(1.0)),
                        ("title".to_string(), KernelValue::from("One")),
                    ])),
                    KernelValue::Object(BTreeMap::from([
                        ("id".to_string(), KernelValue::from(2.0)),
                        ("title".to_string(), KernelValue::from("Two")),
                    ])),
                ]),
            ),
            (SinkPortId(2), KernelValue::from(2.0)),
        ]);

        let host_view =
            materialize_host_view_template(&template, &sink_values).expect("materialized template");
        let root = host_view.root.expect("root");
        let repeated = &root.children[0].children;
        assert_eq!(repeated.len(), 1);
        assert_eq!(repeated[0].retained_key.mapped_item_identity, Some(2));
        assert!(matches!(
            repeated[0].kind,
            HostViewKind::StaticLabel { ref text } if text == "Two"
        ));
    }
}
