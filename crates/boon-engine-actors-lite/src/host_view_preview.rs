use crate::bridge::{
    HostButtonLabel, HostCrossAlign, HostSelectOption, HostStripeDirection, HostTemplatedTextPart,
    HostViewIr, HostViewKind, HostViewMatchArm, HostViewMatchValue, HostViewNode, HostWidth,
};
use crate::host_view_template::kernel_value_truthy;
use crate::interactive_preview::{InteractivePreview, render_interactive_preview};
use crate::ir::{RetainedNodeKey, SinkPortId, SourcePortId};
use boon::platform::browser::kernel::KernelValue;
use boon::zoon::*;
use boon_renderer_zoon::{
    FakeRenderState, RenderInteractionHandlers, render_snapshot_root_with_handlers,
};
use boon_scene::{
    EventPortId, NodeId, RenderDiffBatch, RenderOp, RenderRoot, UiEventBatch, UiEventKind,
    UiFactBatch, UiNode, UiNodeKind,
};
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HostEventBinding {
    pub source_port: SourcePortId,
    pub mapped_item_identity: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct HostEventBindingKey {
    source_port: SourcePortId,
    mapped_item_identity: Option<u64>,
}

#[derive(Debug)]
pub(crate) struct HostViewPreviewApp {
    host_view: HostViewIr,
    sink_values: BTreeMap<SinkPortId, KernelValue>,
    renderer: HostViewPreviewRenderer,
}

impl HostViewPreviewApp {
    #[must_use]
    pub(crate) fn new(
        host_view: HostViewIr,
        sink_values: BTreeMap<SinkPortId, KernelValue>,
    ) -> Self {
        Self {
            host_view,
            sink_values,
            renderer: HostViewPreviewRenderer::default(),
        }
    }

    #[must_use]
    pub(crate) fn render_root(&mut self) -> UiNode {
        self.render_snapshot().0
    }

    #[must_use]
    pub(crate) fn render_snapshot(&mut self) -> (UiNode, FakeRenderState) {
        self.renderer
            .render_snapshot(&self.host_view, &self.sink_values)
    }

    #[must_use]
    pub(crate) fn preview_text(&mut self) -> String {
        preview_text(&self.render_root())
    }

    #[must_use]
    pub(crate) fn sink_value(&self, sink: SinkPortId) -> Option<&KernelValue> {
        self.sink_values.get(&sink)
    }

    pub(crate) fn set_sink_value(&mut self, sink: SinkPortId, value: KernelValue) {
        self.sink_values.insert(sink, value);
    }

    pub(crate) fn set_host_view(&mut self, host_view: HostViewIr) {
        self.host_view = host_view;
    }

    #[must_use]
    pub(crate) fn event_port_for_source(&self, source_port: SourcePortId) -> Option<EventPortId> {
        self.renderer.event_port_for_source(source_port)
    }

    #[must_use]
    #[cfg(test)]
    pub(crate) fn event_port_for_mapped_source(
        &self,
        source_port: SourcePortId,
        mapped_item_identity: u64,
    ) -> Option<EventPortId> {
        self.renderer
            .event_port_for_mapped_source(source_port, mapped_item_identity)
    }

    #[must_use]
    pub(crate) fn event_binding_for_port(
        &self,
        event_port: EventPortId,
    ) -> Option<HostEventBinding> {
        self.renderer.event_binding_for_port(event_port)
    }

    #[must_use]
    pub(crate) fn retained_key_for_node(&self, node_id: NodeId) -> Option<RetainedNodeKey> {
        self.renderer.retained_key_for_node(node_id)
    }

    #[must_use]
    pub(crate) fn retained_nodes(&self) -> &BTreeMap<RetainedNodeKey, NodeId> {
        &self.renderer.retained_nodes
    }
}

pub(crate) trait InteractiveHostViewModel {
    fn app_mut(&mut self) -> &mut HostViewPreviewApp;
    fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool;

    fn dispatch_ui_facts(&mut self, _batch: UiFactBatch) -> bool {
        false
    }

    fn render_snapshot(&mut self) -> (RenderRoot, FakeRenderState) {
        let (root, state) = self.app_mut().render_snapshot();
        (RenderRoot::UiTree(root), state)
    }
}

struct InteractiveHostViewPreview<Model> {
    model: Model,
}

impl<Model> InteractivePreview for InteractiveHostViewPreview<Model>
where
    Model: InteractiveHostViewModel,
{
    fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool {
        self.model.dispatch_ui_events(batch)
    }

    fn dispatch_ui_facts(&mut self, batch: UiFactBatch) -> bool {
        self.model.dispatch_ui_facts(batch)
    }

    fn render_snapshot(&mut self) -> (RenderRoot, FakeRenderState) {
        self.model.render_snapshot()
    }
}

pub(crate) fn render_interactive_host_view<Model>(model: Model) -> impl Element
where
    Model: InteractiveHostViewModel + 'static,
{
    render_interactive_preview(InteractiveHostViewPreview { model })
}

#[derive(Debug, Default)]
struct HostViewPreviewRenderer {
    retained_nodes: BTreeMap<RetainedNodeKey, NodeId>,
    rendered_keys: HashMap<NodeId, RetainedNodeKey>,
    event_ports: BTreeMap<HostEventBindingKey, EventPortId>,
    event_bindings: HashMap<EventPortId, HostEventBinding>,
}

impl HostViewPreviewRenderer {
    #[cfg(test)]
    fn event_port_for_binding(
        &self,
        source_port: SourcePortId,
        mapped_item_identity: Option<u64>,
    ) -> Option<EventPortId> {
        self.event_ports
            .get(&HostEventBindingKey {
                source_port,
                mapped_item_identity,
            })
            .copied()
    }

    fn attach_event_port(
        &mut self,
        ops: &mut Vec<RenderOp>,
        id: NodeId,
        source_port: SourcePortId,
        mapped_item_identity: Option<u64>,
        kind: UiEventKind,
    ) -> EventPortId {
        let event_port = *self
            .event_ports
            .entry(HostEventBindingKey {
                source_port,
                mapped_item_identity,
            })
            .or_insert_with(EventPortId::new);
        self.event_bindings.insert(
            event_port,
            HostEventBinding {
                source_port,
                mapped_item_identity,
            },
        );
        ops.push(RenderOp::AttachEventPort {
            id,
            port: event_port,
            kind,
        });
        event_port
    }

    #[must_use]
    fn event_port_for_source(&self, source_port: SourcePortId) -> Option<EventPortId> {
        let mut matches = self
            .event_bindings
            .iter()
            .filter_map(|(event_port, binding)| {
                (binding.source_port == source_port).then_some(*event_port)
            });
        let first = matches.next()?;
        matches.next().is_none().then_some(first)
    }

    #[must_use]
    #[cfg(test)]
    pub(crate) fn event_port_for_mapped_source(
        &self,
        source_port: SourcePortId,
        mapped_item_identity: u64,
    ) -> Option<EventPortId> {
        self.event_port_for_binding(source_port, Some(mapped_item_identity))
    }

    #[must_use]
    fn event_binding_for_port(&self, event_port: EventPortId) -> Option<HostEventBinding> {
        self.event_bindings.get(&event_port).copied()
    }

    #[must_use]
    fn retained_key_for_node(&self, node_id: NodeId) -> Option<RetainedNodeKey> {
        self.rendered_keys.get(&node_id).copied()
    }

    #[must_use]
    fn render_snapshot(
        &mut self,
        host_view: &HostViewIr,
        sink_values: &BTreeMap<SinkPortId, KernelValue>,
    ) -> (UiNode, FakeRenderState) {
        let root = host_view.root.as_ref().expect("host view root");
        let mut ops = Vec::new();
        let ui_root = self.render_host_node(root, sink_values, &mut ops);
        let mut state = FakeRenderState::default();
        state
            .apply_batch(&RenderDiffBatch { ops })
            .expect("host view render ops should apply");
        (ui_root, state)
    }

    fn render_host_node(
        &mut self,
        node: &HostViewNode,
        sink_values: &BTreeMap<SinkPortId, KernelValue>,
        ops: &mut Vec<RenderOp>,
    ) -> UiNode {
        let id = *self
            .retained_nodes
            .entry(node.retained_key)
            .or_insert_with(NodeId::new);
        self.rendered_keys.insert(id, node.retained_key);

        let mut children = node
            .children
            .iter()
            .map(|child| self.render_host_node(child, sink_values, ops))
            .collect::<Vec<_>>();

        let kind = match &node.kind {
            HostViewKind::Document => {
                apply_document_styles(ops, id);
                UiNodeKind::Element {
                    tag: "div".to_string(),
                    text: None,
                    event_ports: Vec::new(),
                }
            }
            HostViewKind::Container { center_row } => {
                ops.push(RenderOp::SetStyle {
                    id,
                    name: "display".to_string(),
                    value: Some("flex".to_string()),
                });
                if *center_row {
                    ops.push(RenderOp::SetStyle {
                        id,
                        name: "justify-content".to_string(),
                        value: Some("center".to_string()),
                    });
                    ops.push(RenderOp::SetStyle {
                        id,
                        name: "align-items".to_string(),
                        value: Some("center".to_string()),
                    });
                }
                UiNodeKind::Element {
                    tag: "div".to_string(),
                    text: None,
                    event_ports: Vec::new(),
                }
            }
            HostViewKind::AbsolutePanel {
                width_px,
                height_px,
                background,
            } => {
                apply_absolute_panel_styles(ops, id, *width_px, *height_px, background);
                UiNodeKind::Element {
                    tag: "div".to_string(),
                    text: None,
                    event_ports: Vec::new(),
                }
            }
            HostViewKind::Stripe => {
                apply_stripe_styles(ops, id);
                UiNodeKind::Element {
                    tag: "div".to_string(),
                    text: None,
                    event_ports: Vec::new(),
                }
            }
            HostViewKind::StripeLayout {
                direction,
                gap_px,
                padding_px,
                width,
                align_cross,
            } => {
                apply_stripe_layout_styles(
                    ops,
                    id,
                    *direction,
                    *gap_px,
                    *padding_px,
                    width.as_ref(),
                    *align_cross,
                );
                UiNodeKind::Element {
                    tag: "div".to_string(),
                    text: None,
                    event_ports: Vec::new(),
                }
            }
            HostViewKind::Label { sink } => UiNodeKind::Element {
                tag: "span".to_string(),
                text: Some(render_sink_value(
                    sink_values.get(sink).unwrap_or(&KernelValue::Skip),
                )),
                event_ports: Vec::new(),
            },
            HostViewKind::StaticLabel { text } => UiNodeKind::Element {
                tag: "span".to_string(),
                text: Some(text.clone()),
                event_ports: Vec::new(),
            },
            HostViewKind::MatchGroup {
                condition_sink,
                arms,
                fallback_child_count,
            } => {
                children = select_match_group_children(
                    children,
                    sink_values
                        .get(condition_sink)
                        .unwrap_or(&KernelValue::Skip),
                    arms,
                    *fallback_child_count,
                );
                UiNodeKind::Element {
                    tag: "div".to_string(),
                    text: None,
                    event_ports: Vec::new(),
                }
            }
            HostViewKind::ConditionalLabel {
                condition_sink,
                when_true,
                when_false,
            } => UiNodeKind::Element {
                tag: "span".to_string(),
                text: Some(
                    if kernel_value_truthy(
                        sink_values
                            .get(condition_sink)
                            .unwrap_or(&KernelValue::Skip),
                    ) {
                        when_true.clone()
                    } else {
                        when_false.clone()
                    },
                ),
                event_ports: Vec::new(),
            },
            HostViewKind::Paragraph => UiNodeKind::Element {
                tag: "p".to_string(),
                text: None,
                event_ports: Vec::new(),
            },
            HostViewKind::Link { href, new_tab } => {
                ops.push(RenderOp::SetProperty {
                    id,
                    name: "href".to_string(),
                    value: Some(href.clone()),
                });
                if *new_tab {
                    ops.push(RenderOp::SetProperty {
                        id,
                        name: "target".to_string(),
                        value: Some("_blank".to_string()),
                    });
                    ops.push(RenderOp::SetProperty {
                        id,
                        name: "rel".to_string(),
                        value: Some("noopener noreferrer".to_string()),
                    });
                }
                UiNodeKind::Element {
                    tag: "a".to_string(),
                    text: None,
                    event_ports: Vec::new(),
                }
            }
            HostViewKind::TemplatedLabel { parts } => UiNodeKind::Element {
                tag: "span".to_string(),
                text: Some(
                    parts
                        .iter()
                        .map(|part| match part {
                            HostTemplatedTextPart::Static(text) => text.clone(),
                            HostTemplatedTextPart::Sink(sink) => render_sink_value(
                                sink_values.get(sink).unwrap_or(&KernelValue::Skip),
                            ),
                        })
                        .collect::<String>(),
                ),
                event_ports: Vec::new(),
            },
            HostViewKind::StyledLabel {
                sink,
                font_size_px,
                bold,
                color,
            } => {
                apply_label_styles(ops, id, *font_size_px, *bold, color.as_deref());
                UiNodeKind::Element {
                    tag: "span".to_string(),
                    text: Some(render_sink_value(
                        sink_values.get(sink).unwrap_or(&KernelValue::Skip),
                    )),
                    event_ports: Vec::new(),
                }
            }
            HostViewKind::ClickArea {
                click_port,
                width_px,
                height_px,
                background,
            } => {
                let event_port = self.attach_event_port(
                    ops,
                    id,
                    *click_port,
                    node.retained_key.mapped_item_identity,
                    UiEventKind::Click,
                );
                ops.push(RenderOp::SetStyle {
                    id,
                    name: "width".to_string(),
                    value: Some(format!("{width_px}px")),
                });
                ops.push(RenderOp::SetStyle {
                    id,
                    name: "height".to_string(),
                    value: Some(format!("{height_px}px")),
                });
                ops.push(RenderOp::SetStyle {
                    id,
                    name: "background".to_string(),
                    value: Some(background.clone()),
                });
                ops.push(RenderOp::SetStyle {
                    id,
                    name: "display".to_string(),
                    value: Some("block".to_string()),
                });
                ops.push(RenderOp::SetStyle {
                    id,
                    name: "border".to_string(),
                    value: Some("1px solid rgba(255,255,255,0.2)".to_string()),
                });
                UiNodeKind::Element {
                    tag: "div".to_string(),
                    text: None,
                    event_ports: vec![event_port],
                }
            }
            HostViewKind::AbsoluteCanvas {
                click_port,
                width_px,
                height_px,
                background,
            } => {
                let event_port = self.attach_event_port(
                    ops,
                    id,
                    *click_port,
                    node.retained_key.mapped_item_identity,
                    UiEventKind::Click,
                );
                ops.push(RenderOp::SetStyle {
                    id,
                    name: "width".to_string(),
                    value: Some(format!("{width_px}px")),
                });
                ops.push(RenderOp::SetStyle {
                    id,
                    name: "height".to_string(),
                    value: Some(format!("{height_px}px")),
                });
                ops.push(RenderOp::SetStyle {
                    id,
                    name: "background".to_string(),
                    value: Some(background.clone()),
                });
                ops.push(RenderOp::SetStyle {
                    id,
                    name: "display".to_string(),
                    value: Some("block".to_string()),
                });
                ops.push(RenderOp::SetStyle {
                    id,
                    name: "position".to_string(),
                    value: Some("relative".to_string()),
                });
                ops.push(RenderOp::SetStyle {
                    id,
                    name: "overflow".to_string(),
                    value: Some("hidden".to_string()),
                });
                ops.push(RenderOp::SetStyle {
                    id,
                    name: "border".to_string(),
                    value: Some("1px solid rgba(255,255,255,0.2)".to_string()),
                });
                UiNodeKind::Element {
                    tag: "div".to_string(),
                    text: None,
                    event_ports: vec![event_port],
                }
            }
            HostViewKind::PositionedCircleList { .. } => UiNodeKind::Element {
                tag: "div".to_string(),
                text: None,
                event_ports: Vec::new(),
            },
            HostViewKind::PositionedBox {
                x_px,
                y_px,
                width_px,
                height_px,
                padding_px,
                background,
                rounded_px,
                text_color,
            } => {
                apply_positioned_box_styles(
                    ops,
                    id,
                    *x_px,
                    *y_px,
                    *width_px,
                    *height_px,
                    *padding_px,
                    background.as_deref(),
                    *rounded_px,
                    text_color.as_deref(),
                );
                UiNodeKind::Element {
                    tag: "div".to_string(),
                    text: None,
                    event_ports: Vec::new(),
                }
            }
            HostViewKind::ActionLabel {
                sink,
                press_port,
                event_kind,
            } => {
                let event_port = self.attach_event_port(
                    ops,
                    id,
                    *press_port,
                    node.retained_key.mapped_item_identity,
                    event_kind.clone(),
                );
                UiNodeKind::Element {
                    tag: "span".to_string(),
                    text: Some(render_sink_value(
                        sink_values.get(sink).unwrap_or(&KernelValue::Skip),
                    )),
                    event_ports: vec![event_port],
                }
            }
            HostViewKind::StaticActionLabel {
                text,
                press_port,
                event_kind,
            } => {
                let event_port = self.attach_event_port(
                    ops,
                    id,
                    *press_port,
                    node.retained_key.mapped_item_identity,
                    event_kind.clone(),
                );
                UiNodeKind::Element {
                    tag: "span".to_string(),
                    text: Some(text.clone()),
                    event_ports: vec![event_port],
                }
            }
            HostViewKind::StyledActionLabel {
                sink,
                press_port,
                event_kind,
                width,
                bold_sink,
            } => {
                let event_port = self.attach_event_port(
                    ops,
                    id,
                    *press_port,
                    node.retained_key.mapped_item_identity,
                    event_kind.clone(),
                );
                apply_button_styles(ops, id, width.as_ref(), None, false, None, None, None);
                if sink_is_truthy(bold_sink, sink_values) {
                    set_style(ops, id, "font-weight", "700");
                }
                UiNodeKind::Element {
                    tag: "button".to_string(),
                    text: Some(render_sink_value(
                        sink_values.get(sink).unwrap_or(&KernelValue::Skip),
                    )),
                    event_ports: vec![event_port],
                }
            }
            HostViewKind::Checkbox {
                checked_sink,
                click_port,
            } => {
                let click_event_port = self.attach_event_port(
                    ops,
                    id,
                    *click_port,
                    node.retained_key.mapped_item_identity,
                    UiEventKind::Click,
                );
                let checked = match sink_values.get(checked_sink) {
                    Some(KernelValue::Bool(value)) => *value,
                    Some(KernelValue::Text(text)) | Some(KernelValue::Tag(text)) => text == "true",
                    _ => false,
                };
                ops.push(RenderOp::SetProperty {
                    id,
                    name: "type".to_string(),
                    value: Some("checkbox".to_string()),
                });
                apply_checkbox_styles(ops, id);
                ops.push(RenderOp::SetProperty {
                    id,
                    name: "role".to_string(),
                    value: Some("checkbox".to_string()),
                });
                ops.push(RenderOp::SetProperty {
                    id,
                    name: "checked".to_string(),
                    value: if checked {
                        Some("true".to_string())
                    } else {
                        None
                    },
                });
                ops.push(RenderOp::SetChecked { id, checked });
                UiNodeKind::Element {
                    tag: "input".to_string(),
                    text: None,
                    event_ports: vec![click_event_port],
                }
            }
            HostViewKind::StaticCheckbox {
                checked,
                click_port,
                labelled_by_view_site,
            } => {
                let click_event_port = self.attach_event_port(
                    ops,
                    id,
                    *click_port,
                    node.retained_key.mapped_item_identity,
                    UiEventKind::Click,
                );
                ops.push(RenderOp::SetProperty {
                    id,
                    name: "type".to_string(),
                    value: Some("checkbox".to_string()),
                });
                apply_checkbox_styles(ops, id);
                ops.push(RenderOp::SetProperty {
                    id,
                    name: "role".to_string(),
                    value: Some("checkbox".to_string()),
                });
                if let Some(view_site) = labelled_by_view_site {
                    ops.push(RenderOp::SetProperty {
                        id,
                        name: "data-label-ref-view-site".to_string(),
                        value: Some(view_site.0.to_string()),
                    });
                }
                ops.push(RenderOp::SetProperty {
                    id,
                    name: "checked".to_string(),
                    value: if *checked {
                        Some("true".to_string())
                    } else {
                        None
                    },
                });
                ops.push(RenderOp::SetChecked {
                    id,
                    checked: *checked,
                });
                UiNodeKind::Element {
                    tag: "input".to_string(),
                    text: None,
                    event_ports: vec![click_event_port],
                }
            }
            HostViewKind::TextInput {
                value_sink,
                placeholder,
                change_port,
                key_down_port,
                blur_port,
                focus_port,
                focus_on_mount,
                disabled_sink,
            } => {
                let mapped_item_identity = node.retained_key.mapped_item_identity;
                let input_event_port = self.attach_event_port(
                    ops,
                    id,
                    *change_port,
                    mapped_item_identity,
                    UiEventKind::Input,
                );
                let key_down_event_port = self.attach_event_port(
                    ops,
                    id,
                    *key_down_port,
                    mapped_item_identity,
                    UiEventKind::KeyDown,
                );
                let blur_event_port = blur_port.map(|port| {
                    self.attach_event_port(ops, id, port, mapped_item_identity, UiEventKind::Blur)
                });
                let focus_event_port = focus_port.map(|port| {
                    self.attach_event_port(ops, id, port, mapped_item_identity, UiEventKind::Focus)
                });
                ops.push(RenderOp::SetProperty {
                    id,
                    name: "type".to_string(),
                    value: Some("text".to_string()),
                });
                ops.push(RenderOp::SetProperty {
                    id,
                    name: "placeholder".to_string(),
                    value: Some(placeholder.clone()),
                });
                apply_text_input_styles(ops, id, None);
                if *focus_on_mount {
                    ops.push(RenderOp::SetProperty {
                        id,
                        name: "autofocus".to_string(),
                        value: Some("true".to_string()),
                    });
                }
                apply_disabled_state(ops, id, sink_is_truthy(disabled_sink, sink_values));
                ops.push(RenderOp::SetInputValue {
                    id,
                    value: render_sink_value(
                        sink_values.get(value_sink).unwrap_or(&KernelValue::Skip),
                    ),
                });
                UiNodeKind::Element {
                    tag: "input".to_string(),
                    text: None,
                    event_ports: vec![
                        Some(input_event_port),
                        Some(key_down_event_port),
                        blur_event_port,
                        focus_event_port,
                    ]
                    .into_iter()
                    .flatten()
                    .collect(),
                }
            }
            HostViewKind::StaticTextInput {
                value,
                placeholder,
                change_port,
                key_down_port,
                blur_port,
                focus_port,
                focus_on_mount,
                disabled,
            } => {
                let mapped_item_identity = node.retained_key.mapped_item_identity;
                let input_event_port = self.attach_event_port(
                    ops,
                    id,
                    *change_port,
                    mapped_item_identity,
                    UiEventKind::Input,
                );
                let key_down_event_port = self.attach_event_port(
                    ops,
                    id,
                    *key_down_port,
                    mapped_item_identity,
                    UiEventKind::KeyDown,
                );
                let blur_event_port = blur_port.map(|port| {
                    self.attach_event_port(ops, id, port, mapped_item_identity, UiEventKind::Blur)
                });
                let focus_event_port = focus_port.map(|port| {
                    self.attach_event_port(ops, id, port, mapped_item_identity, UiEventKind::Focus)
                });
                ops.push(RenderOp::SetProperty {
                    id,
                    name: "type".to_string(),
                    value: Some("text".to_string()),
                });
                ops.push(RenderOp::SetProperty {
                    id,
                    name: "placeholder".to_string(),
                    value: Some(placeholder.clone()),
                });
                apply_text_input_styles(ops, id, None);
                if *focus_on_mount {
                    ops.push(RenderOp::SetProperty {
                        id,
                        name: "autofocus".to_string(),
                        value: Some("true".to_string()),
                    });
                }
                apply_disabled_state(ops, id, *disabled);
                ops.push(RenderOp::SetInputValue {
                    id,
                    value: value.clone(),
                });
                UiNodeKind::Element {
                    tag: "input".to_string(),
                    text: None,
                    event_ports: vec![
                        Some(input_event_port),
                        Some(key_down_event_port),
                        blur_event_port,
                        focus_event_port,
                    ]
                    .into_iter()
                    .flatten()
                    .collect(),
                }
            }
            HostViewKind::StyledTextInput {
                value_sink,
                placeholder,
                change_port,
                key_down_port,
                blur_port,
                focus_port,
                focus_on_mount,
                disabled_sink,
                width,
            } => {
                let mapped_item_identity = node.retained_key.mapped_item_identity;
                let input_event_port = self.attach_event_port(
                    ops,
                    id,
                    *change_port,
                    mapped_item_identity,
                    UiEventKind::Input,
                );
                let key_down_event_port = self.attach_event_port(
                    ops,
                    id,
                    *key_down_port,
                    mapped_item_identity,
                    UiEventKind::KeyDown,
                );
                let blur_event_port = blur_port.map(|port| {
                    self.attach_event_port(ops, id, port, mapped_item_identity, UiEventKind::Blur)
                });
                let focus_event_port = focus_port.map(|port| {
                    self.attach_event_port(ops, id, port, mapped_item_identity, UiEventKind::Focus)
                });
                ops.push(RenderOp::SetProperty {
                    id,
                    name: "type".to_string(),
                    value: Some("text".to_string()),
                });
                ops.push(RenderOp::SetProperty {
                    id,
                    name: "placeholder".to_string(),
                    value: Some(placeholder.clone()),
                });
                apply_text_input_styles(ops, id, width.as_ref());
                if *focus_on_mount {
                    ops.push(RenderOp::SetProperty {
                        id,
                        name: "autofocus".to_string(),
                        value: Some("true".to_string()),
                    });
                }
                apply_disabled_state(ops, id, sink_is_truthy(disabled_sink, sink_values));
                ops.push(RenderOp::SetInputValue {
                    id,
                    value: render_sink_value(
                        sink_values.get(value_sink).unwrap_or(&KernelValue::Skip),
                    ),
                });
                UiNodeKind::Element {
                    tag: "input".to_string(),
                    text: None,
                    event_ports: vec![
                        Some(input_event_port),
                        Some(key_down_event_port),
                        blur_event_port,
                        focus_event_port,
                    ]
                    .into_iter()
                    .flatten()
                    .collect(),
                }
            }
            HostViewKind::Slider {
                value_sink,
                input_port,
                min,
                max,
                step,
                disabled_sink,
            } => {
                let input_event_port = self.attach_event_port(
                    ops,
                    id,
                    *input_port,
                    node.retained_key.mapped_item_identity,
                    UiEventKind::Change,
                );
                ops.push(RenderOp::SetProperty {
                    id,
                    name: "type".to_string(),
                    value: Some("range".to_string()),
                });
                ops.push(RenderOp::SetProperty {
                    id,
                    name: "min".to_string(),
                    value: Some(min.clone()),
                });
                ops.push(RenderOp::SetProperty {
                    id,
                    name: "max".to_string(),
                    value: Some(max.clone()),
                });
                ops.push(RenderOp::SetProperty {
                    id,
                    name: "step".to_string(),
                    value: Some(step.clone()),
                });
                apply_slider_styles(ops, id, None);
                apply_disabled_state(ops, id, sink_is_truthy(disabled_sink, sink_values));
                ops.push(RenderOp::SetInputValue {
                    id,
                    value: render_sink_value(
                        sink_values.get(value_sink).unwrap_or(&KernelValue::Skip),
                    ),
                });
                UiNodeKind::Element {
                    tag: "input".to_string(),
                    text: None,
                    event_ports: vec![input_event_port],
                }
            }
            HostViewKind::StyledSlider {
                value_sink,
                input_port,
                min,
                max,
                step,
                disabled_sink,
                width,
            } => {
                let input_event_port = self.attach_event_port(
                    ops,
                    id,
                    *input_port,
                    node.retained_key.mapped_item_identity,
                    UiEventKind::Change,
                );
                ops.push(RenderOp::SetProperty {
                    id,
                    name: "type".to_string(),
                    value: Some("range".to_string()),
                });
                ops.push(RenderOp::SetProperty {
                    id,
                    name: "min".to_string(),
                    value: Some(min.clone()),
                });
                ops.push(RenderOp::SetProperty {
                    id,
                    name: "max".to_string(),
                    value: Some(max.clone()),
                });
                ops.push(RenderOp::SetProperty {
                    id,
                    name: "step".to_string(),
                    value: Some(step.clone()),
                });
                apply_slider_styles(ops, id, width.as_ref());
                apply_disabled_state(ops, id, sink_is_truthy(disabled_sink, sink_values));
                ops.push(RenderOp::SetInputValue {
                    id,
                    value: render_sink_value(
                        sink_values.get(value_sink).unwrap_or(&KernelValue::Skip),
                    ),
                });
                UiNodeKind::Element {
                    tag: "input".to_string(),
                    text: None,
                    event_ports: vec![input_event_port],
                }
            }
            HostViewKind::Select {
                selected_sink,
                change_port,
                options,
                disabled_sink,
            } => {
                let input_event_port = self.attach_event_port(
                    ops,
                    id,
                    *change_port,
                    node.retained_key.mapped_item_identity,
                    UiEventKind::Input,
                );
                apply_select_styles(ops, id, None);
                ops.push(RenderOp::SetInputValue {
                    id,
                    value: render_sink_value(
                        sink_values.get(selected_sink).unwrap_or(&KernelValue::Skip),
                    ),
                });
                apply_disabled_state(ops, id, sink_is_truthy(disabled_sink, sink_values));
                children = options
                    .iter()
                    .enumerate()
                    .map(|(index, option)| self.render_select_option(node, index, option, ops))
                    .collect::<Vec<_>>();
                UiNodeKind::Element {
                    tag: "select".to_string(),
                    text: None,
                    event_ports: vec![input_event_port],
                }
            }
            HostViewKind::StyledSelect {
                selected_sink,
                change_port,
                options,
                disabled_sink,
                width,
            } => {
                let input_event_port = self.attach_event_port(
                    ops,
                    id,
                    *change_port,
                    node.retained_key.mapped_item_identity,
                    UiEventKind::Input,
                );
                apply_select_styles(ops, id, width.as_ref());
                ops.push(RenderOp::SetInputValue {
                    id,
                    value: render_sink_value(
                        sink_values.get(selected_sink).unwrap_or(&KernelValue::Skip),
                    ),
                });
                apply_disabled_state(ops, id, sink_is_truthy(disabled_sink, sink_values));
                children = options
                    .iter()
                    .enumerate()
                    .map(|(index, option)| self.render_select_option(node, index, option, ops))
                    .collect::<Vec<_>>();
                UiNodeKind::Element {
                    tag: "select".to_string(),
                    text: None,
                    event_ports: vec![input_event_port],
                }
            }
            HostViewKind::Button {
                label,
                press_port,
                disabled_sink,
            } => {
                let disabled = sink_is_truthy(disabled_sink, sink_values);
                let event_port = self.attach_event_port(
                    ops,
                    id,
                    *press_port,
                    node.retained_key.mapped_item_identity,
                    UiEventKind::Click,
                );
                apply_button_styles(ops, id, None, None, false, None, None, None);
                apply_disabled_state(ops, id, disabled);
                if !disabled {
                    set_style(ops, id, "cursor", "pointer");
                }
                UiNodeKind::Element {
                    tag: "button".to_string(),
                    text: Some(render_button_label(label, sink_values)),
                    event_ports: vec![event_port],
                }
            }
            HostViewKind::StyledButton {
                label,
                press_port,
                disabled_sink,
                width,
                padding_px,
                rounded_fully,
                background,
                background_sink,
                active_background,
                outline_sink,
                active_outline,
            } => {
                let disabled = sink_is_truthy(disabled_sink, sink_values);
                let event_port = self.attach_event_port(
                    ops,
                    id,
                    *press_port,
                    node.retained_key.mapped_item_identity,
                    UiEventKind::Click,
                );
                apply_button_styles(
                    ops,
                    id,
                    width.as_ref(),
                    *padding_px,
                    *rounded_fully,
                    background.as_deref(),
                    if sink_is_truthy(background_sink, sink_values) {
                        active_background.as_deref()
                    } else {
                        None
                    },
                    if sink_is_truthy(outline_sink, sink_values) {
                        active_outline.as_deref()
                    } else {
                        None
                    },
                );
                apply_disabled_state(ops, id, disabled);
                if !disabled {
                    set_style(ops, id, "cursor", "pointer");
                }
                UiNodeKind::Element {
                    tag: "button".to_string(),
                    text: Some(render_button_label(label, sink_values)),
                    event_ports: vec![event_port],
                }
            }
            HostViewKind::TimerSource {
                tick_port,
                interval_ms,
            } => {
                let event_port = self.attach_event_port(
                    ops,
                    id,
                    *tick_port,
                    node.retained_key.mapped_item_identity,
                    UiEventKind::Custom(format!("timer:{interval_ms}")),
                );
                ops.push(RenderOp::SetProperty {
                    id,
                    name: "aria-hidden".to_string(),
                    value: Some("true".to_string()),
                });
                UiNodeKind::Element {
                    tag: "span".to_string(),
                    text: None,
                    event_ports: vec![event_port],
                }
            }
            HostViewKind::GenericElement {
                tag,
                text,
                properties,
                styles,
                input_value,
                checked,
                event_bindings,
            } => {
                let mut event_ports = Vec::with_capacity(event_bindings.len());
                for binding in event_bindings {
                    event_ports.push(self.attach_event_port(
                        ops,
                        id,
                        binding.source_port,
                        node.retained_key.mapped_item_identity,
                        binding.event_kind.clone(),
                    ));
                }
                for (name, value) in properties {
                    ops.push(RenderOp::SetProperty {
                        id,
                        name: name.clone(),
                        value: value.clone(),
                    });
                }
                for (name, value) in styles {
                    ops.push(RenderOp::SetStyle {
                        id,
                        name: name.clone(),
                        value: value.clone(),
                    });
                }
                if let Some(value) = input_value {
                    ops.push(RenderOp::SetInputValue {
                        id,
                        value: value.clone(),
                    });
                }
                if let Some(checked) = checked {
                    ops.push(RenderOp::SetChecked {
                        id,
                        checked: *checked,
                    });
                }
                UiNodeKind::Element {
                    tag: tag.clone(),
                    text: text.clone(),
                    event_ports,
                }
            }
        };

        if let HostViewKind::PositionedCircleList {
            circles_sink,
            radius_px,
            fill,
            stroke,
            stroke_width_px,
        } = &node.kind
        {
            children = self.render_circle_canvas_children(
                node,
                *circles_sink,
                *radius_px,
                fill,
                stroke,
                *stroke_width_px,
                sink_values,
                ops,
            );
        }

        UiNode { id, kind, children }
    }

    fn render_select_option(
        &mut self,
        parent: &HostViewNode,
        index: usize,
        option: &HostSelectOption,
        ops: &mut Vec<RenderOp>,
    ) -> UiNode {
        let key = RetainedNodeKey {
            view_site: crate::ir::ViewSiteId(
                parent.retained_key.view_site.0 + 1_000_000 + index as u32,
            ),
            function_instance: parent.retained_key.function_instance,
            mapped_item_identity: parent.retained_key.mapped_item_identity,
        };
        let id = *self.retained_nodes.entry(key).or_insert_with(NodeId::new);
        self.rendered_keys.insert(id, key);
        ops.push(RenderOp::SetProperty {
            id,
            name: "value".to_string(),
            value: Some(option.value.clone()),
        });
        UiNode {
            id,
            kind: UiNodeKind::Element {
                tag: "option".to_string(),
                text: Some(option.label.clone()),
                event_ports: Vec::new(),
            },
            children: Vec::new(),
        }
    }

    fn render_circle_canvas_children(
        &mut self,
        parent: &HostViewNode,
        circles_sink: SinkPortId,
        radius_px: u32,
        fill: &str,
        stroke: &str,
        stroke_width_px: u32,
        sink_values: &BTreeMap<SinkPortId, KernelValue>,
        ops: &mut Vec<RenderOp>,
    ) -> Vec<UiNode> {
        let Some(KernelValue::List(circles)) = sink_values.get(&circles_sink) else {
            return Vec::new();
        };
        circles
            .iter()
            .enumerate()
            .filter_map(|(index, circle)| {
                let KernelValue::Object(fields) = circle else {
                    return None;
                };
                let x = circle_coord(fields, "x")?;
                let y = circle_coord(fields, "y")?;
                Some(self.render_circle_canvas_circle(
                    parent,
                    index,
                    x,
                    y,
                    radius_px,
                    fill,
                    stroke,
                    stroke_width_px,
                    ops,
                ))
            })
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_circle_canvas_circle(
        &mut self,
        parent: &HostViewNode,
        index: usize,
        x: f64,
        y: f64,
        radius_px: u32,
        fill: &str,
        stroke: &str,
        stroke_width_px: u32,
        ops: &mut Vec<RenderOp>,
    ) -> UiNode {
        let key = RetainedNodeKey {
            view_site: crate::ir::ViewSiteId(
                parent.retained_key.view_site.0 + 2_000_000 + index as u32,
            ),
            function_instance: parent.retained_key.function_instance,
            mapped_item_identity: Some(index as u64),
        };
        let id = *self.retained_nodes.entry(key).or_insert_with(NodeId::new);
        let diameter_px = radius_px * 2;
        set_style(ops, id, "position", "absolute");
        set_style(ops, id, "left", &format!("{}px", x - f64::from(radius_px)));
        set_style(ops, id, "top", &format!("{}px", y - f64::from(radius_px)));
        set_style(ops, id, "width", &format!("{diameter_px}px"));
        set_style(ops, id, "height", &format!("{diameter_px}px"));
        set_style(ops, id, "border-radius", "9999px");
        set_style(ops, id, "background", fill);
        set_style(
            ops,
            id,
            "border",
            &format!("{stroke_width_px}px solid {stroke}"),
        );
        set_style(ops, id, "box-sizing", "border-box");
        set_style(ops, id, "pointer-events", "none");
        UiNode {
            id,
            kind: UiNodeKind::Element {
                tag: "div".to_string(),
                text: None,
                event_ports: Vec::new(),
            },
            children: Vec::new(),
        }
    }
}

#[must_use]
fn preview_text(node: &UiNode) -> String {
    fn collect(node: &UiNode, out: &mut String) {
        match &node.kind {
            UiNodeKind::Element { text, .. } => {
                if let Some(text) = text {
                    out.push_str(text);
                }
            }
            UiNodeKind::Text { text } => out.push_str(text),
        }
        for child in &node.children {
            collect(child, out);
        }
    }

    let mut out = String::new();
    collect(node, &mut out);
    out
}

pub(crate) fn render_static_host_view(app: HostViewPreviewApp) -> impl Element {
    let handlers = RenderInteractionHandlers::new(|_batch| {}, |_facts| {});
    let mut app = app;
    let (root, state) = app.render_snapshot();
    render_snapshot_root_with_handlers(&RenderRoot::UiTree(root), &state, &handlers)
}

fn render_sink_value(value: &KernelValue) -> String {
    match value {
        KernelValue::Number(number) if number.fract() == 0.0 => format!("{}", *number as i64),
        KernelValue::Number(number) => number.to_string(),
        KernelValue::Text(text) | KernelValue::Tag(text) => text.clone(),
        KernelValue::Bool(value) => {
            if *value {
                "True".to_string()
            } else {
                "False".to_string()
            }
        }
        KernelValue::Skip => String::new(),
        KernelValue::Object(_) | KernelValue::List(_) => format!("{value:?}"),
    }
}

fn render_button_label(
    label: &HostButtonLabel,
    sink_values: &BTreeMap<SinkPortId, KernelValue>,
) -> String {
    match label {
        HostButtonLabel::Static(text) => text.clone(),
        HostButtonLabel::Sink(sink) => {
            render_sink_value(sink_values.get(sink).unwrap_or(&KernelValue::Skip))
        }
        HostButtonLabel::Templated(parts) => parts
            .iter()
            .map(|part| match part {
                HostTemplatedTextPart::Static(text) => text.clone(),
                HostTemplatedTextPart::Sink(sink) => {
                    render_sink_value(sink_values.get(sink).unwrap_or(&KernelValue::Skip))
                }
            })
            .collect(),
    }
}

fn sink_is_truthy(
    sink: &Option<SinkPortId>,
    sink_values: &BTreeMap<SinkPortId, KernelValue>,
) -> bool {
    let Some(sink) = sink else {
        return false;
    };
    match sink_values.get(sink) {
        Some(KernelValue::Bool(value)) => *value,
        Some(KernelValue::Number(value)) => *value != 0.0,
        Some(KernelValue::Text(text)) | Some(KernelValue::Tag(text)) => {
            matches!(text.as_str(), "true" | "1")
        }
        _ => false,
    }
}

fn apply_disabled_state(ops: &mut Vec<RenderOp>, id: NodeId, disabled: bool) {
    let value = disabled.then(|| "true".to_string());
    ops.push(RenderOp::SetProperty {
        id,
        name: "disabled".to_string(),
        value: value.clone(),
    });
    ops.push(RenderOp::SetProperty {
        id,
        name: "aria-disabled".to_string(),
        value,
    });
    ops.push(RenderOp::SetStyle {
        id,
        name: "opacity".to_string(),
        value: disabled.then(|| "0.55".to_string()),
    });
    ops.push(RenderOp::SetStyle {
        id,
        name: "cursor".to_string(),
        value: disabled.then(|| "not-allowed".to_string()),
    });
    ops.push(RenderOp::SetStyle {
        id,
        name: "filter".to_string(),
        value: disabled.then(|| "saturate(0.6)".to_string()),
    });
}

fn select_match_group_children(
    children: Vec<UiNode>,
    condition_value: &KernelValue,
    arms: &[HostViewMatchArm],
    fallback_child_count: usize,
) -> Vec<UiNode> {
    let mut arm_offsets = Vec::with_capacity(arms.len());
    let mut start = 0usize;
    for arm in arms {
        arm_offsets.push((start, arm.child_count));
        start += arm.child_count;
    }

    if let Some((index, _)) = arms
        .iter()
        .enumerate()
        .find(|(_, arm)| host_match_value(condition_value, &arm.matcher))
    {
        let (start, count) = arm_offsets[index];
        return children.into_iter().skip(start).take(count).collect();
    }

    children
        .into_iter()
        .skip(start)
        .take(fallback_child_count)
        .collect()
}

fn host_match_value(value: &KernelValue, matcher: &HostViewMatchValue) -> bool {
    match (value, matcher) {
        (KernelValue::Bool(value), HostViewMatchValue::Bool(expected)) => value == expected,
        (KernelValue::Text(value), HostViewMatchValue::Text(expected)) => value == expected,
        (KernelValue::Tag(value), HostViewMatchValue::Tag(expected)) => value == expected,
        _ => false,
    }
}

fn set_style(ops: &mut Vec<RenderOp>, id: NodeId, name: &str, value: &str) {
    ops.push(RenderOp::SetStyle {
        id,
        name: name.to_string(),
        value: Some(value.to_string()),
    });
}

fn apply_document_styles(ops: &mut Vec<RenderOp>, id: NodeId) {
    set_style(ops, id, "display", "flex");
    set_style(ops, id, "flex-direction", "column");
    set_style(ops, id, "align-items", "flex-start");
    set_style(ops, id, "gap", "16px");
    set_style(ops, id, "padding", "16px");
    set_style(ops, id, "box-sizing", "border-box");
    set_style(
        ops,
        id,
        "font-family",
        "ui-sans-serif, system-ui, sans-serif",
    );
}

fn apply_absolute_panel_styles(
    ops: &mut Vec<RenderOp>,
    id: NodeId,
    width_px: u32,
    height_px: u32,
    background: &str,
) {
    set_style(ops, id, "position", "relative");
    set_style(ops, id, "display", "block");
    set_style(ops, id, "width", &format!("{width_px}px"));
    set_style(ops, id, "height", &format!("{height_px}px"));
    set_style(ops, id, "background", background);
    set_style(ops, id, "overflow", "hidden");
    set_style(ops, id, "box-sizing", "border-box");
}

fn apply_stripe_styles(ops: &mut Vec<RenderOp>, id: NodeId) {
    apply_stripe_layout_styles(ops, id, HostStripeDirection::Column, 12, None, None, None);
}

fn apply_stripe_layout_styles(
    ops: &mut Vec<RenderOp>,
    id: NodeId,
    direction: HostStripeDirection,
    gap_px: u32,
    padding_px: Option<u32>,
    width: Option<&HostWidth>,
    align_cross: Option<HostCrossAlign>,
) {
    set_style(ops, id, "display", "flex");
    set_style(
        ops,
        id,
        "flex-direction",
        match direction {
            HostStripeDirection::Row => "row",
            HostStripeDirection::Column => "column",
        },
    );
    set_style(
        ops,
        id,
        "align-items",
        match align_cross.unwrap_or(HostCrossAlign::Start) {
            HostCrossAlign::Start => "flex-start",
            HostCrossAlign::Center => "center",
        },
    );
    set_style(ops, id, "gap", &format!("{gap_px}px"));
    if let Some(padding_px) = padding_px {
        set_style(ops, id, "padding", &format!("{padding_px}px"));
    }
    apply_width_style(ops, id, width);
    set_style(ops, id, "box-sizing", "border-box");
}

fn apply_label_styles(
    ops: &mut Vec<RenderOp>,
    id: NodeId,
    font_size_px: Option<u32>,
    bold: bool,
    color: Option<&str>,
) {
    if let Some(font_size_px) = font_size_px {
        set_style(ops, id, "font-size", &format!("{font_size_px}px"));
    }
    if bold {
        set_style(ops, id, "font-weight", "700");
    }
    if let Some(color) = color {
        set_style(ops, id, "color", color);
    }
}

fn apply_positioned_box_styles(
    ops: &mut Vec<RenderOp>,
    id: NodeId,
    x_px: u32,
    y_px: u32,
    width_px: u32,
    height_px: u32,
    padding_px: Option<u32>,
    background: Option<&str>,
    rounded_px: Option<u32>,
    text_color: Option<&str>,
) {
    set_style(ops, id, "position", "absolute");
    set_style(ops, id, "left", &format!("{x_px}px"));
    set_style(ops, id, "top", &format!("{y_px}px"));
    set_style(ops, id, "width", &format!("{width_px}px"));
    set_style(ops, id, "height", &format!("{height_px}px"));
    set_style(ops, id, "box-sizing", "border-box");
    if let Some(padding_px) = padding_px {
        set_style(ops, id, "padding", &format!("{padding_px}px"));
    }
    if let Some(background) = background {
        set_style(ops, id, "background", background);
    }
    if let Some(rounded_px) = rounded_px {
        set_style(ops, id, "border-radius", &format!("{rounded_px}px"));
    }
    if let Some(text_color) = text_color {
        set_style(ops, id, "color", text_color);
    }
}

fn apply_width_style(ops: &mut Vec<RenderOp>, id: NodeId, width: Option<&HostWidth>) {
    let Some(width) = width else {
        return;
    };
    match width {
        HostWidth::Px(px) => set_style(ops, id, "width", &format!("{px}px")),
        HostWidth::Fill => set_style(ops, id, "width", "100%"),
    }
}

fn apply_button_styles(
    ops: &mut Vec<RenderOp>,
    id: NodeId,
    width: Option<&HostWidth>,
    padding_px: Option<u32>,
    rounded_fully: bool,
    background: Option<&str>,
    active_background: Option<&str>,
    active_outline: Option<&str>,
) {
    match padding_px {
        Some(padding_px) => set_style(ops, id, "padding", &format!("{padding_px}px")),
        None => set_style(ops, id, "padding", "8px 12px"),
    }
    set_style(ops, id, "border", "1px solid rgba(148, 163, 184, 0.7)");
    set_style(
        ops,
        id,
        "border-radius",
        if rounded_fully { "9999px" } else { "10px" },
    );
    set_style(
        ops,
        id,
        "background",
        active_background.unwrap_or(background.unwrap_or("rgba(241, 245, 249, 0.92)")),
    );
    set_style(ops, id, "outline", active_outline.unwrap_or("none"));
    set_style(ops, id, "cursor", "pointer");
    set_style(ops, id, "font", "inherit");
    apply_width_style(ops, id, width);
}

fn circle_coord(fields: &BTreeMap<String, KernelValue>, field: &str) -> Option<f64> {
    match fields.get(field) {
        Some(KernelValue::Number(value)) => Some(*value),
        Some(KernelValue::Text(value)) | Some(KernelValue::Tag(value)) => value.parse().ok(),
        _ => None,
    }
}

fn apply_text_input_styles(ops: &mut Vec<RenderOp>, id: NodeId, width: Option<&HostWidth>) {
    set_style(ops, id, "padding", "8px 10px");
    set_style(ops, id, "border", "1px solid rgba(148, 163, 184, 0.7)");
    set_style(ops, id, "border-radius", "10px");
    set_style(ops, id, "background", "rgba(255, 255, 255, 0.96)");
    set_style(ops, id, "font", "inherit");
    match width {
        Some(width) => apply_width_style(ops, id, Some(width)),
        None => set_style(ops, id, "min-width", "220px"),
    }
    set_style(ops, id, "box-sizing", "border-box");
}

fn apply_slider_styles(ops: &mut Vec<RenderOp>, id: NodeId, width: Option<&HostWidth>) {
    match width {
        Some(width) => apply_width_style(ops, id, Some(width)),
        None => set_style(ops, id, "min-width", "220px"),
    }
}

fn apply_select_styles(ops: &mut Vec<RenderOp>, id: NodeId, width: Option<&HostWidth>) {
    set_style(ops, id, "padding", "8px 10px");
    set_style(ops, id, "border", "1px solid rgba(148, 163, 184, 0.7)");
    set_style(ops, id, "border-radius", "10px");
    set_style(ops, id, "background", "rgba(255, 255, 255, 0.96)");
    set_style(ops, id, "font", "inherit");
    apply_width_style(ops, id, width);
}

fn apply_checkbox_styles(ops: &mut Vec<RenderOp>, id: NodeId) {
    set_style(ops, id, "width", "18px");
    set_style(ops, id, "height", "18px");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::{HostViewKind, HostViewNode};
    use crate::ir::{FunctionInstanceId, RetainedNodeKey, ViewSiteId};

    #[test]
    fn preserves_retained_ids_across_rerenders() {
        let retained_key = RetainedNodeKey {
            view_site: ViewSiteId(1),
            function_instance: Some(FunctionInstanceId(1)),
            mapped_item_identity: None,
        };
        let host_view = HostViewIr {
            root: Some(HostViewNode {
                retained_key,
                kind: HostViewKind::Label {
                    sink: SinkPortId(1),
                },
                children: Vec::new(),
            }),
        };
        let mut renderer = HostViewPreviewRenderer::default();
        let mut sinks = BTreeMap::new();
        sinks.insert(SinkPortId(1), KernelValue::from("A"));
        let first = renderer.render_snapshot(&host_view, &sinks).0.id;
        sinks.insert(SinkPortId(1), KernelValue::from("B"));
        let second = renderer.render_snapshot(&host_view, &sinks).0.id;
        assert_eq!(first, second);
    }

    #[test]
    fn preview_app_updates_sink_values_without_replacing_retained_root() {
        let retained_key = RetainedNodeKey {
            view_site: ViewSiteId(10),
            function_instance: Some(FunctionInstanceId(20)),
            mapped_item_identity: None,
        };
        let host_view = HostViewIr {
            root: Some(HostViewNode {
                retained_key,
                kind: HostViewKind::Label {
                    sink: SinkPortId(7),
                },
                children: Vec::new(),
            }),
        };
        let mut sinks = BTreeMap::new();
        sinks.insert(SinkPortId(7), KernelValue::from("before"));
        let mut app = HostViewPreviewApp::new(host_view, sinks);

        let first = app.render_root();
        app.set_sink_value(SinkPortId(7), KernelValue::from("after"));
        let second = app.render_root();

        assert_eq!(first.id, second.id);
        assert_eq!(preview_text(&second), "after");
    }

    #[test]
    fn preview_app_replaces_host_view_without_replacing_matching_retained_root() {
        let retained_key = RetainedNodeKey {
            view_site: ViewSiteId(11),
            function_instance: Some(FunctionInstanceId(21)),
            mapped_item_identity: None,
        };
        let mut app = HostViewPreviewApp::new(
            HostViewIr {
                root: Some(HostViewNode {
                    retained_key,
                    kind: HostViewKind::Label {
                        sink: SinkPortId(8),
                    },
                    children: Vec::new(),
                }),
            },
            BTreeMap::from([(SinkPortId(8), KernelValue::from("before"))]),
        );

        let first = app.render_root();
        app.set_host_view(HostViewIr {
            root: Some(HostViewNode {
                retained_key,
                kind: HostViewKind::TemplatedLabel {
                    parts: vec![
                        HostTemplatedTextPart::Static("[".to_string()),
                        HostTemplatedTextPart::Sink(SinkPortId(8)),
                        HostTemplatedTextPart::Static("]".to_string()),
                    ],
                },
                children: Vec::new(),
            }),
        });
        app.set_sink_value(SinkPortId(8), KernelValue::from("after"));
        let second = app.render_root();

        assert_eq!(first.id, second.id);
        assert_eq!(preview_text(&second), "[after]");
    }

    #[test]
    fn checkbox_host_nodes_publish_click_port_and_checked_state() {
        let retained_key = RetainedNodeKey {
            view_site: ViewSiteId(3),
            function_instance: Some(FunctionInstanceId(3)),
            mapped_item_identity: None,
        };
        let host_view = HostViewIr {
            root: Some(HostViewNode {
                retained_key,
                kind: HostViewKind::Checkbox {
                    checked_sink: SinkPortId(7),
                    click_port: SourcePortId(8),
                },
                children: Vec::new(),
            }),
        };
        let mut sinks = BTreeMap::new();
        sinks.insert(SinkPortId(7), KernelValue::Bool(true));
        let mut renderer = HostViewPreviewRenderer::default();
        let (root, _state) = renderer.render_snapshot(&host_view, &sinks);

        assert_eq!(
            renderer.event_port_for_source(SourcePortId(8)).is_some(),
            true
        );
        assert_eq!(
            renderer.event_binding_for_port(
                renderer
                    .event_port_for_source(SourcePortId(8))
                    .expect("checkbox event port"),
            ),
            Some(HostEventBinding {
                source_port: SourcePortId(8),
                mapped_item_identity: None,
            })
        );
        assert!(matches!(root.kind, UiNodeKind::Element { ref tag, .. } if tag == "input"));
    }

    #[test]
    fn mapped_items_publish_distinct_event_ports_per_item_identity() {
        let host_view = HostViewIr {
            root: Some(HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(40),
                    function_instance: Some(FunctionInstanceId(40)),
                    mapped_item_identity: None,
                },
                kind: HostViewKind::Stripe,
                children: vec![
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(41),
                            function_instance: Some(FunctionInstanceId(40)),
                            mapped_item_identity: Some(1),
                        },
                        kind: HostViewKind::Button {
                            label: HostButtonLabel::Static("A".to_string()),
                            press_port: SourcePortId(20),
                            disabled_sink: None,
                        },
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(41),
                            function_instance: Some(FunctionInstanceId(40)),
                            mapped_item_identity: Some(2),
                        },
                        kind: HostViewKind::Button {
                            label: HostButtonLabel::Static("B".to_string()),
                            press_port: SourcePortId(20),
                            disabled_sink: None,
                        },
                        children: Vec::new(),
                    },
                ],
            }),
        };
        let mut renderer = HostViewPreviewRenderer::default();
        let (_root, _state) = renderer.render_snapshot(&host_view, &BTreeMap::new());

        let first = renderer
            .event_port_for_mapped_source(SourcePortId(20), 1)
            .expect("first mapped event port");
        let second = renderer
            .event_port_for_mapped_source(SourcePortId(20), 2)
            .expect("second mapped event port");
        assert_ne!(first, second);
        assert_eq!(
            renderer.event_binding_for_port(first),
            Some(HostEventBinding {
                source_port: SourcePortId(20),
                mapped_item_identity: Some(1),
            })
        );
        assert_eq!(
            renderer.event_binding_for_port(second),
            Some(HostEventBinding {
                source_port: SourcePortId(20),
                mapped_item_identity: Some(2),
            })
        );
    }

    #[test]
    fn click_area_host_nodes_publish_click_port_and_styles() {
        let retained_key = RetainedNodeKey {
            view_site: ViewSiteId(3),
            function_instance: Some(FunctionInstanceId(30)),
            mapped_item_identity: None,
        };
        let host_view = HostViewIr {
            root: Some(HostViewNode {
                retained_key,
                kind: HostViewKind::ClickArea {
                    click_port: SourcePortId(16),
                    width_px: 460,
                    height_px: 300,
                    background: "rgba(255,255,255,0.1)".to_string(),
                },
                children: Vec::new(),
            }),
        };
        let mut renderer = HostViewPreviewRenderer::default();
        let (root, _state) = renderer.render_snapshot(&host_view, &BTreeMap::new());

        assert!(renderer.event_port_for_source(SourcePortId(16)).is_some());
        assert!(matches!(root.kind, UiNodeKind::Element { ref tag, .. } if tag == "div"));
    }

    #[test]
    fn action_label_host_nodes_publish_click_port_and_text() {
        let retained_key = RetainedNodeKey {
            view_site: ViewSiteId(4),
            function_instance: Some(FunctionInstanceId(4)),
            mapped_item_identity: None,
        };
        let host_view = HostViewIr {
            root: Some(HostViewNode {
                retained_key,
                kind: HostViewKind::ActionLabel {
                    sink: SinkPortId(9),
                    press_port: SourcePortId(10),
                    event_kind: UiEventKind::Click,
                },
                children: Vec::new(),
            }),
        };
        let mut sinks = BTreeMap::new();
        sinks.insert(SinkPortId(9), KernelValue::from("Clickable"));
        let mut renderer = HostViewPreviewRenderer::default();
        let (root, _state) = renderer.render_snapshot(&host_view, &sinks);

        assert!(renderer.event_port_for_source(SourcePortId(10)).is_some());
        assert!(matches!(
            root.kind,
            UiNodeKind::Element {
                ref tag,
                ref text,
                ..
            } if tag == "span" && text.as_deref() == Some("Clickable")
        ));
    }

    #[test]
    fn action_label_host_nodes_publish_configured_double_click_port() {
        let retained_key = RetainedNodeKey {
            view_site: ViewSiteId(5),
            function_instance: Some(FunctionInstanceId(5)),
            mapped_item_identity: None,
        };
        let host_view = HostViewIr {
            root: Some(HostViewNode {
                retained_key,
                kind: HostViewKind::ActionLabel {
                    sink: SinkPortId(10),
                    press_port: SourcePortId(11),
                    event_kind: UiEventKind::DoubleClick,
                },
                children: Vec::new(),
            }),
        };
        let mut sinks = BTreeMap::new();
        sinks.insert(SinkPortId(10), KernelValue::from("Double"));
        let mut renderer = HostViewPreviewRenderer::default();
        let (root, state) = renderer.render_snapshot(&host_view, &sinks);

        assert_eq!(
            state.event_ports_for(root.id),
            vec![(
                renderer
                    .event_port_for_source(SourcePortId(11))
                    .expect("double-click event port"),
                UiEventKind::DoubleClick
            )]
        );
    }

    #[test]
    fn select_host_nodes_publish_input_port_and_option_labels() {
        let retained_key = RetainedNodeKey {
            view_site: ViewSiteId(6),
            function_instance: Some(FunctionInstanceId(6)),
            mapped_item_identity: None,
        };
        let host_view = HostViewIr {
            root: Some(HostViewNode {
                retained_key,
                kind: HostViewKind::Select {
                    selected_sink: SinkPortId(11),
                    change_port: SourcePortId(12),
                    options: vec![
                        HostSelectOption {
                            value: "one-way".to_string(),
                            label: "One-way flight".to_string(),
                        },
                        HostSelectOption {
                            value: "return".to_string(),
                            label: "Return flight".to_string(),
                        },
                    ],
                    disabled_sink: None,
                },
                children: Vec::new(),
            }),
        };
        let mut sinks = BTreeMap::new();
        sinks.insert(SinkPortId(11), KernelValue::from("one-way"));
        let mut renderer = HostViewPreviewRenderer::default();
        let (root, _state) = renderer.render_snapshot(&host_view, &sinks);

        assert!(renderer.event_port_for_source(SourcePortId(12)).is_some());
        assert!(matches!(root.kind, UiNodeKind::Element { ref tag, .. } if tag == "select"));
        assert_eq!(preview_text(&root), "One-way flightReturn flight");
    }

    #[test]
    fn slider_host_nodes_publish_input_port_and_range_type() {
        let retained_key = RetainedNodeKey {
            view_site: ViewSiteId(7),
            function_instance: Some(FunctionInstanceId(7)),
            mapped_item_identity: None,
        };
        let host_view = HostViewIr {
            root: Some(HostViewNode {
                retained_key,
                kind: HostViewKind::Slider {
                    value_sink: SinkPortId(13),
                    input_port: SourcePortId(14),
                    min: "1".to_string(),
                    max: "30".to_string(),
                    step: "0.1".to_string(),
                    disabled_sink: None,
                },
                children: Vec::new(),
            }),
        };
        let mut sinks = BTreeMap::new();
        sinks.insert(SinkPortId(13), KernelValue::from("15"));
        let mut renderer = HostViewPreviewRenderer::default();
        let (root, _state) = renderer.render_snapshot(&host_view, &sinks);

        assert!(renderer.event_port_for_source(SourcePortId(14)).is_some());
        assert!(matches!(root.kind, UiNodeKind::Element { ref tag, .. } if tag == "input"));
    }

    #[test]
    fn timer_source_nodes_publish_custom_timer_port() {
        let retained_key = RetainedNodeKey {
            view_site: ViewSiteId(8),
            function_instance: Some(FunctionInstanceId(8)),
            mapped_item_identity: None,
        };
        let host_view = HostViewIr {
            root: Some(HostViewNode {
                retained_key,
                kind: HostViewKind::TimerSource {
                    tick_port: SourcePortId(15),
                    interval_ms: 100,
                },
                children: Vec::new(),
            }),
        };
        let mut renderer = HostViewPreviewRenderer::default();
        let (root, _state) = renderer.render_snapshot(&host_view, &BTreeMap::new());

        assert!(renderer.event_port_for_source(SourcePortId(15)).is_some());
        assert!(matches!(root.kind, UiNodeKind::Element { ref tag, .. } if tag == "span"));
    }

    #[test]
    fn document_and_stripe_nodes_emit_layout_styles() {
        let root_key = RetainedNodeKey {
            view_site: ViewSiteId(50),
            function_instance: Some(FunctionInstanceId(50)),
            mapped_item_identity: None,
        };
        let child_key = RetainedNodeKey {
            view_site: ViewSiteId(51),
            function_instance: Some(FunctionInstanceId(50)),
            mapped_item_identity: None,
        };
        let host_view = HostViewIr {
            root: Some(HostViewNode {
                retained_key: root_key,
                kind: HostViewKind::Document,
                children: vec![HostViewNode {
                    retained_key: child_key,
                    kind: HostViewKind::Stripe,
                    children: Vec::new(),
                }],
            }),
        };
        let mut renderer = HostViewPreviewRenderer::default();
        let (root, state) = renderer.render_snapshot(&host_view, &BTreeMap::new());
        let child = root.children.first().expect("stripe child");

        assert_eq!(state.style_value(root.id, "display"), Some("flex"));
        assert_eq!(state.style_value(root.id, "padding"), Some("16px"));
        assert_eq!(state.style_value(child.id, "display"), Some("flex"));
        assert_eq!(state.style_value(child.id, "gap"), Some("12px"));
    }

    #[test]
    fn stripe_layout_nodes_emit_direction_and_gap_styles() {
        let retained_key = RetainedNodeKey {
            view_site: ViewSiteId(52),
            function_instance: Some(FunctionInstanceId(52)),
            mapped_item_identity: None,
        };
        let host_view = HostViewIr {
            root: Some(HostViewNode {
                retained_key,
                kind: HostViewKind::StripeLayout {
                    direction: HostStripeDirection::Row,
                    gap_px: 15,
                    padding_px: Some(20),
                    width: Some(HostWidth::Px(300)),
                    align_cross: Some(HostCrossAlign::Center),
                },
                children: Vec::new(),
            }),
        };
        let mut renderer = HostViewPreviewRenderer::default();
        let (root, state) = renderer.render_snapshot(&host_view, &BTreeMap::new());

        assert_eq!(state.style_value(root.id, "display"), Some("flex"));
        assert_eq!(state.style_value(root.id, "flex-direction"), Some("row"));
        assert_eq!(state.style_value(root.id, "gap"), Some("15px"));
        assert_eq!(state.style_value(root.id, "padding"), Some("20px"));
        assert_eq!(state.style_value(root.id, "width"), Some("300px"));
        assert_eq!(state.style_value(root.id, "align-items"), Some("center"));
    }

    #[test]
    fn controls_emit_baseline_bridge_styles() {
        let root_key = RetainedNodeKey {
            view_site: ViewSiteId(60),
            function_instance: Some(FunctionInstanceId(60)),
            mapped_item_identity: None,
        };
        let input_key = RetainedNodeKey {
            view_site: ViewSiteId(61),
            function_instance: Some(FunctionInstanceId(60)),
            mapped_item_identity: None,
        };
        let button_key = RetainedNodeKey {
            view_site: ViewSiteId(62),
            function_instance: Some(FunctionInstanceId(60)),
            mapped_item_identity: None,
        };
        let host_view = HostViewIr {
            root: Some(HostViewNode {
                retained_key: root_key,
                kind: HostViewKind::Document,
                children: vec![
                    HostViewNode {
                        retained_key: input_key,
                        kind: HostViewKind::TextInput {
                            value_sink: SinkPortId(1),
                            placeholder: "Name".to_string(),
                            change_port: SourcePortId(1),
                            key_down_port: SourcePortId(2),
                            blur_port: None,
                            focus_port: None,
                            focus_on_mount: false,
                            disabled_sink: None,
                        },
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: button_key,
                        kind: HostViewKind::Button {
                            label: HostButtonLabel::Static("Create".to_string()),
                            press_port: SourcePortId(3),
                            disabled_sink: None,
                        },
                        children: Vec::new(),
                    },
                ],
            }),
        };
        let mut sinks = BTreeMap::new();
        sinks.insert(SinkPortId(1), KernelValue::from(""));
        let mut renderer = HostViewPreviewRenderer::default();
        let (root, state) = renderer.render_snapshot(&host_view, &sinks);
        let input = &root.children[0];
        let button = &root.children[1];

        assert_eq!(state.style_value(input.id, "padding"), Some("8px 10px"));
        assert_eq!(state.style_value(input.id, "min-width"), Some("220px"));
        assert_eq!(state.style_value(button.id, "border-radius"), Some("10px"));
        assert_eq!(state.style_value(button.id, "cursor"), Some("pointer"));
    }

    #[test]
    fn text_inputs_publish_optional_focus_and_blur_ports() {
        let retained_key = RetainedNodeKey {
            view_site: ViewSiteId(66),
            function_instance: Some(FunctionInstanceId(66)),
            mapped_item_identity: None,
        };
        let host_view = HostViewIr {
            root: Some(HostViewNode {
                retained_key,
                kind: HostViewKind::TextInput {
                    value_sink: SinkPortId(33),
                    placeholder: "Focus".to_string(),
                    change_port: SourcePortId(33),
                    key_down_port: SourcePortId(34),
                    blur_port: Some(SourcePortId(35)),
                    focus_port: Some(SourcePortId(36)),
                    focus_on_mount: false,
                    disabled_sink: None,
                },
                children: Vec::new(),
            }),
        };
        let mut renderer = HostViewPreviewRenderer::default();
        let (root, _state) = renderer.render_snapshot(&host_view, &BTreeMap::new());

        let UiNodeKind::Element { event_ports, .. } = root.kind else {
            panic!("text input should render as element");
        };
        assert_eq!(event_ports.len(), 4);
        assert!(renderer.event_port_for_source(SourcePortId(33)).is_some());
        assert!(renderer.event_port_for_source(SourcePortId(34)).is_some());
        assert!(renderer.event_port_for_source(SourcePortId(35)).is_some());
        assert!(renderer.event_port_for_source(SourcePortId(36)).is_some());
    }

    #[test]
    fn disabled_controls_emit_visual_disabled_styles() {
        let root_key = RetainedNodeKey {
            view_site: ViewSiteId(63),
            function_instance: Some(FunctionInstanceId(63)),
            mapped_item_identity: None,
        };
        let input_key = RetainedNodeKey {
            view_site: ViewSiteId(64),
            function_instance: Some(FunctionInstanceId(63)),
            mapped_item_identity: None,
        };
        let button_key = RetainedNodeKey {
            view_site: ViewSiteId(65),
            function_instance: Some(FunctionInstanceId(63)),
            mapped_item_identity: None,
        };
        let host_view = HostViewIr {
            root: Some(HostViewNode {
                retained_key: root_key,
                kind: HostViewKind::Document,
                children: vec![
                    HostViewNode {
                        retained_key: input_key,
                        kind: HostViewKind::StyledTextInput {
                            value_sink: SinkPortId(30),
                            placeholder: "Disabled".to_string(),
                            change_port: SourcePortId(30),
                            key_down_port: SourcePortId(31),
                            blur_port: None,
                            focus_port: None,
                            focus_on_mount: false,
                            disabled_sink: Some(SinkPortId(31)),
                            width: Some(HostWidth::Fill),
                        },
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: button_key,
                        kind: HostViewKind::StyledButton {
                            label: HostButtonLabel::Static("Book".to_string()),
                            press_port: SourcePortId(32),
                            disabled_sink: Some(SinkPortId(32)),
                            width: Some(HostWidth::Fill),
                            padding_px: None,
                            rounded_fully: false,
                            background: None,
                            background_sink: None,
                            active_background: None,
                            outline_sink: None,
                            active_outline: None,
                        },
                        children: Vec::new(),
                    },
                ],
            }),
        };
        let mut sinks = BTreeMap::new();
        sinks.insert(SinkPortId(30), KernelValue::from(""));
        sinks.insert(SinkPortId(31), KernelValue::Bool(true));
        sinks.insert(SinkPortId(32), KernelValue::Bool(true));
        let mut renderer = HostViewPreviewRenderer::default();
        let (root, state) = renderer.render_snapshot(&host_view, &sinks);
        let input = &root.children[0];
        let button = &root.children[1];

        assert_eq!(state.property_value(input.id, "disabled"), Some("true"));
        assert_eq!(state.property_value(button.id, "disabled"), Some("true"));
        assert_eq!(state.style_value(input.id, "opacity"), Some("0.55"));
        assert_eq!(state.style_value(button.id, "opacity"), Some("0.55"));
        assert_eq!(state.style_value(input.id, "cursor"), Some("not-allowed"));
        assert_eq!(state.style_value(button.id, "cursor"), Some("not-allowed"));
    }

    #[test]
    fn styled_controls_and_labels_emit_source_styles() {
        let root_key = RetainedNodeKey {
            view_site: ViewSiteId(70),
            function_instance: Some(FunctionInstanceId(70)),
            mapped_item_identity: None,
        };
        let label_key = RetainedNodeKey {
            view_site: ViewSiteId(71),
            function_instance: Some(FunctionInstanceId(70)),
            mapped_item_identity: None,
        };
        let input_key = RetainedNodeKey {
            view_site: ViewSiteId(72),
            function_instance: Some(FunctionInstanceId(70)),
            mapped_item_identity: None,
        };
        let select_key = RetainedNodeKey {
            view_site: ViewSiteId(73),
            function_instance: Some(FunctionInstanceId(70)),
            mapped_item_identity: None,
        };
        let slider_key = RetainedNodeKey {
            view_site: ViewSiteId(74),
            function_instance: Some(FunctionInstanceId(70)),
            mapped_item_identity: None,
        };
        let button_key = RetainedNodeKey {
            view_site: ViewSiteId(75),
            function_instance: Some(FunctionInstanceId(70)),
            mapped_item_identity: None,
        };
        let host_view = HostViewIr {
            root: Some(HostViewNode {
                retained_key: root_key,
                kind: HostViewKind::Document,
                children: vec![
                    HostViewNode {
                        retained_key: label_key,
                        kind: HostViewKind::StyledLabel {
                            sink: SinkPortId(10),
                            font_size_px: Some(24),
                            bold: true,
                            color: Some("white".to_string()),
                        },
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: input_key,
                        kind: HostViewKind::StyledTextInput {
                            value_sink: SinkPortId(11),
                            placeholder: "Name".to_string(),
                            change_port: SourcePortId(10),
                            key_down_port: SourcePortId(11),
                            blur_port: None,
                            focus_port: None,
                            focus_on_mount: false,
                            disabled_sink: None,
                            width: Some(HostWidth::Fill),
                        },
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: select_key,
                        kind: HostViewKind::StyledSelect {
                            selected_sink: SinkPortId(12),
                            change_port: SourcePortId(12),
                            options: vec![HostSelectOption {
                                value: "one-way".to_string(),
                                label: "One-way".to_string(),
                            }],
                            disabled_sink: None,
                            width: Some(HostWidth::Fill),
                        },
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: slider_key,
                        kind: HostViewKind::StyledSlider {
                            value_sink: SinkPortId(13),
                            input_port: SourcePortId(13),
                            min: "1".to_string(),
                            max: "30".to_string(),
                            step: "0.1".to_string(),
                            disabled_sink: None,
                            width: Some(HostWidth::Px(200)),
                        },
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: button_key,
                        kind: HostViewKind::StyledButton {
                            label: HostButtonLabel::Static("Go".to_string()),
                            press_port: SourcePortId(14),
                            disabled_sink: None,
                            width: Some(HostWidth::Px(45)),
                            padding_px: Some(10),
                            rounded_fully: true,
                            background: Some("oklch(0.75 0.07 320)".to_string()),
                            background_sink: None,
                            active_background: None,
                            outline_sink: None,
                            active_outline: None,
                        },
                        children: Vec::new(),
                    },
                ],
            }),
        };
        let mut sinks = BTreeMap::new();
        sinks.insert(SinkPortId(10), KernelValue::from("Styled"));
        sinks.insert(SinkPortId(11), KernelValue::from(""));
        sinks.insert(SinkPortId(12), KernelValue::from("one-way"));
        sinks.insert(SinkPortId(13), KernelValue::from("15"));
        let mut renderer = HostViewPreviewRenderer::default();
        let (root, state) = renderer.render_snapshot(&host_view, &sinks);
        let label = &root.children[0];
        let input = &root.children[1];
        let select = &root.children[2];
        let slider = &root.children[3];
        let button = &root.children[4];

        assert_eq!(state.style_value(label.id, "font-size"), Some("24px"));
        assert_eq!(state.style_value(label.id, "font-weight"), Some("700"));
        assert_eq!(state.style_value(label.id, "color"), Some("white"));
        assert_eq!(state.style_value(input.id, "width"), Some("100%"));
        assert_eq!(state.style_value(select.id, "width"), Some("100%"));
        assert_eq!(state.style_value(slider.id, "width"), Some("200px"));
        assert_eq!(state.style_value(button.id, "width"), Some("45px"));
        assert_eq!(state.style_value(button.id, "padding"), Some("10px"));
        assert_eq!(
            state.style_value(button.id, "border-radius"),
            Some("9999px")
        );
    }

    #[test]
    fn styled_action_label_uses_sink_text_width_and_dynamic_bold() {
        let retained_key = RetainedNodeKey {
            view_site: ViewSiteId(80),
            function_instance: Some(FunctionInstanceId(80)),
            mapped_item_identity: None,
        };
        let host_view = HostViewIr {
            root: Some(HostViewNode {
                retained_key,
                kind: HostViewKind::StyledActionLabel {
                    sink: SinkPortId(20),
                    press_port: SourcePortId(20),
                    event_kind: UiEventKind::Click,
                    width: Some(HostWidth::Fill),
                    bold_sink: Some(SinkPortId(21)),
                },
                children: Vec::new(),
            }),
        };
        let mut sinks = BTreeMap::new();
        sinks.insert(SinkPortId(20), KernelValue::from("Mustermann, Max"));
        sinks.insert(SinkPortId(21), KernelValue::Bool(true));
        let mut renderer = HostViewPreviewRenderer::default();
        let (root, state) = renderer.render_snapshot(&host_view, &sinks);

        assert!(matches!(root.kind, UiNodeKind::Element { ref tag, .. } if tag == "button"));
        assert_eq!(state.style_value(root.id, "width"), Some("100%"));
        assert_eq!(state.style_value(root.id, "font-weight"), Some("700"));
    }

    #[test]
    fn styled_button_uses_sink_driven_active_background() {
        let retained_key = RetainedNodeKey {
            view_site: ViewSiteId(81),
            function_instance: Some(FunctionInstanceId(81)),
            mapped_item_identity: None,
        };
        let host_view = HostViewIr {
            root: Some(HostViewNode {
                retained_key,
                kind: HostViewKind::StyledButton {
                    label: HostButtonLabel::Static("+".to_string()),
                    press_port: SourcePortId(21),
                    disabled_sink: None,
                    width: Some(HostWidth::Px(45)),
                    padding_px: None,
                    rounded_fully: true,
                    background: Some("oklch(0.75 0.07 320)".to_string()),
                    background_sink: Some(SinkPortId(22)),
                    active_background: Some("oklch(0.85 0.07 320)".to_string()),
                    outline_sink: None,
                    active_outline: None,
                },
                children: Vec::new(),
            }),
        };
        let mut renderer = HostViewPreviewRenderer::default();

        let mut idle_sinks = BTreeMap::new();
        idle_sinks.insert(SinkPortId(22), KernelValue::Bool(false));
        let (idle_root, idle_state) = renderer.render_snapshot(&host_view, &idle_sinks);
        assert_eq!(
            idle_state.style_value(idle_root.id, "background"),
            Some("oklch(0.75 0.07 320)")
        );

        let mut active_sinks = BTreeMap::new();
        active_sinks.insert(SinkPortId(22), KernelValue::Bool(true));
        let (active_root, active_state) = renderer.render_snapshot(&host_view, &active_sinks);
        assert_eq!(
            active_state.style_value(active_root.id, "background"),
            Some("oklch(0.85 0.07 320)")
        );
    }

    #[test]
    fn circle_canvas_renders_positioned_circle_children_from_sink() {
        let retained_key = RetainedNodeKey {
            view_site: ViewSiteId(82),
            function_instance: Some(FunctionInstanceId(82)),
            mapped_item_identity: None,
        };
        let host_view = HostViewIr {
            root: Some(HostViewNode {
                retained_key,
                kind: HostViewKind::AbsoluteCanvas {
                    click_port: SourcePortId(30),
                    width_px: 460,
                    height_px: 300,
                    background: "rgba(255,255,255,0.1)".to_string(),
                },
                children: vec![HostViewNode {
                    retained_key: RetainedNodeKey {
                        view_site: ViewSiteId(83),
                        function_instance: Some(FunctionInstanceId(82)),
                        mapped_item_identity: None,
                    },
                    kind: HostViewKind::PositionedCircleList {
                        circles_sink: SinkPortId(30),
                        radius_px: 20,
                        fill: "#3498db".to_string(),
                        stroke: "#2c3e50".to_string(),
                        stroke_width_px: 2,
                    },
                    children: Vec::new(),
                }],
            }),
        };
        let mut sinks = BTreeMap::new();
        sinks.insert(
            SinkPortId(30),
            KernelValue::List(vec![
                KernelValue::Object(BTreeMap::from([
                    ("x".to_string(), KernelValue::from(120.0)),
                    ("y".to_string(), KernelValue::from(80.0)),
                ])),
                KernelValue::Object(BTreeMap::from([
                    ("x".to_string(), KernelValue::from(200.0)),
                    ("y".to_string(), KernelValue::from(160.0)),
                ])),
            ]),
        );
        let mut renderer = HostViewPreviewRenderer::default();
        let (root, state) = renderer.render_snapshot(&host_view, &sinks);
        assert_eq!(root.children.len(), 1);
        let circle_list = &root.children[0];
        assert_eq!(circle_list.children.len(), 2);
        assert_eq!(state.style_value(root.id, "position"), Some("relative"));
        assert_eq!(
            state.style_value(circle_list.children[0].id, "left"),
            Some("100px")
        );
        assert_eq!(
            state.style_value(circle_list.children[0].id, "top"),
            Some("60px")
        );
        assert_eq!(
            state.style_value(circle_list.children[0].id, "background"),
            Some("#3498db")
        );
        assert_eq!(
            state.style_value(circle_list.children[1].id, "left"),
            Some("180px")
        );
        assert_eq!(
            state.style_value(circle_list.children[1].id, "top"),
            Some("140px")
        );
    }

    #[test]
    fn absolute_panel_and_positioned_box_emit_layer_styles() {
        let host_view = HostViewIr {
            root: Some(HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(400),
                    function_instance: Some(FunctionInstanceId(400)),
                    mapped_item_identity: None,
                },
                kind: HostViewKind::Document,
                children: vec![HostViewNode {
                    retained_key: RetainedNodeKey {
                        view_site: ViewSiteId(401),
                        function_instance: Some(FunctionInstanceId(400)),
                        mapped_item_identity: None,
                    },
                    kind: HostViewKind::AbsolutePanel {
                        width_px: 300,
                        height_px: 250,
                        background: "rgb(12, 16, 24)".to_string(),
                    },
                    children: vec![HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(402),
                            function_instance: Some(FunctionInstanceId(400)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::PositionedBox {
                            x_px: 20,
                            y_px: 30,
                            width_px: 180,
                            height_px: 120,
                            padding_px: Some(12),
                            background: Some("rgb(240, 90, 70)".to_string()),
                            rounded_px: Some(8),
                            text_color: Some("white".to_string()),
                        },
                        children: vec![HostViewNode {
                            retained_key: RetainedNodeKey {
                                view_site: ViewSiteId(403),
                                function_instance: Some(FunctionInstanceId(400)),
                                mapped_item_identity: None,
                            },
                            kind: HostViewKind::Label {
                                sink: SinkPortId(400),
                            },
                            children: Vec::new(),
                        }],
                    }],
                }],
            }),
        };
        let sinks = BTreeMap::from([(SinkPortId(400), KernelValue::from("Red Card"))]);
        let mut renderer = HostViewPreviewRenderer::default();
        let (root, state) = renderer.render_snapshot(&host_view, &sinks);
        let panel = &root.children[0];
        let card = &panel.children[0];

        assert_eq!(preview_text(&root), "Red Card");
        assert_eq!(state.style_value(panel.id, "position"), Some("relative"));
        assert_eq!(state.style_value(panel.id, "width"), Some("300px"));
        assert_eq!(state.style_value(panel.id, "height"), Some("250px"));
        assert_eq!(state.style_value(card.id, "position"), Some("absolute"));
        assert_eq!(state.style_value(card.id, "left"), Some("20px"));
        assert_eq!(state.style_value(card.id, "top"), Some("30px"));
        assert_eq!(state.style_value(card.id, "border-radius"), Some("8px"));
        assert_eq!(state.style_value(card.id, "color"), Some("white"));
    }
}
