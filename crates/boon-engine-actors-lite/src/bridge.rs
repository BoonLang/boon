use crate::ids::ActorId;
use crate::ir::{MirrorCellId, RetainedNodeKey, SinkPortId, SourcePortId, ViewSiteId};
use crate::runtime::{Msg, RuntimeCore};
use crate::semantics::CausalSeq;
use boon::platform::browser::kernel::KernelValue;
use boon_scene::UiEventKind;

/// Passive retained host/view structure.
///
/// Reactivity lives in `RuntimeCore`; this layer just binds retained nodes
/// to sink values and can be diffed after one quiescence cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSelectOption {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostElementEventBinding {
    pub source_port: SourcePortId,
    pub event_kind: UiEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostViewMatchValue {
    Bool(bool),
    Text(String),
    Tag(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostViewMatchArm {
    pub matcher: HostViewMatchValue,
    pub child_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostStripeDirection {
    Row,
    Column,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostWidth {
    Px(u32),
    Fill,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostCrossAlign {
    Start,
    Center,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostViewKind {
    Document,
    Stripe,
    Container {
        center_row: bool,
    },
    AbsolutePanel {
        width_px: u32,
        height_px: u32,
        background: String,
    },
    StripeLayout {
        direction: HostStripeDirection,
        gap_px: u32,
        padding_px: Option<u32>,
        width: Option<HostWidth>,
        align_cross: Option<HostCrossAlign>,
    },
    Label {
        sink: SinkPortId,
    },
    StaticLabel {
        text: String,
    },
    MatchGroup {
        condition_sink: SinkPortId,
        arms: Vec<HostViewMatchArm>,
        fallback_child_count: usize,
    },
    ConditionalLabel {
        condition_sink: SinkPortId,
        when_true: String,
        when_false: String,
    },
    Paragraph,
    Link {
        href: String,
        new_tab: bool,
    },
    TemplatedLabel {
        parts: Vec<HostTemplatedTextPart>,
    },
    StyledLabel {
        sink: SinkPortId,
        font_size_px: Option<u32>,
        bold: bool,
        color: Option<String>,
    },
    ClickArea {
        click_port: SourcePortId,
        width_px: u32,
        height_px: u32,
        background: String,
    },
    AbsoluteCanvas {
        click_port: SourcePortId,
        width_px: u32,
        height_px: u32,
        background: String,
    },
    PositionedCircleList {
        circles_sink: SinkPortId,
        radius_px: u32,
        fill: String,
        stroke: String,
        stroke_width_px: u32,
    },
    PositionedBox {
        x_px: u32,
        y_px: u32,
        width_px: u32,
        height_px: u32,
        padding_px: Option<u32>,
        background: Option<String>,
        rounded_px: Option<u32>,
        text_color: Option<String>,
    },
    ActionLabel {
        sink: SinkPortId,
        press_port: SourcePortId,
        event_kind: UiEventKind,
    },
    StaticActionLabel {
        text: String,
        press_port: SourcePortId,
        event_kind: UiEventKind,
    },
    StyledActionLabel {
        sink: SinkPortId,
        press_port: SourcePortId,
        event_kind: UiEventKind,
        width: Option<HostWidth>,
        bold_sink: Option<SinkPortId>,
    },
    Checkbox {
        checked_sink: SinkPortId,
        click_port: SourcePortId,
    },
    StaticCheckbox {
        checked: bool,
        click_port: SourcePortId,
        labelled_by_view_site: Option<ViewSiteId>,
    },
    TextInput {
        value_sink: SinkPortId,
        placeholder: String,
        change_port: SourcePortId,
        key_down_port: SourcePortId,
        blur_port: Option<SourcePortId>,
        focus_port: Option<SourcePortId>,
        focus_on_mount: bool,
        disabled_sink: Option<SinkPortId>,
    },
    StaticTextInput {
        value: String,
        placeholder: String,
        change_port: SourcePortId,
        key_down_port: SourcePortId,
        blur_port: Option<SourcePortId>,
        focus_port: Option<SourcePortId>,
        focus_on_mount: bool,
        disabled: bool,
    },
    StyledTextInput {
        value_sink: SinkPortId,
        placeholder: String,
        change_port: SourcePortId,
        key_down_port: SourcePortId,
        blur_port: Option<SourcePortId>,
        focus_port: Option<SourcePortId>,
        focus_on_mount: bool,
        disabled_sink: Option<SinkPortId>,
        width: Option<HostWidth>,
    },
    Slider {
        value_sink: SinkPortId,
        input_port: SourcePortId,
        min: String,
        max: String,
        step: String,
        disabled_sink: Option<SinkPortId>,
    },
    StyledSlider {
        value_sink: SinkPortId,
        input_port: SourcePortId,
        min: String,
        max: String,
        step: String,
        disabled_sink: Option<SinkPortId>,
        width: Option<HostWidth>,
    },
    Select {
        selected_sink: SinkPortId,
        change_port: SourcePortId,
        options: Vec<HostSelectOption>,
        disabled_sink: Option<SinkPortId>,
    },
    StyledSelect {
        selected_sink: SinkPortId,
        change_port: SourcePortId,
        options: Vec<HostSelectOption>,
        disabled_sink: Option<SinkPortId>,
        width: Option<HostWidth>,
    },
    Button {
        label: HostButtonLabel,
        press_port: SourcePortId,
        disabled_sink: Option<SinkPortId>,
    },
    StyledButton {
        label: HostButtonLabel,
        press_port: SourcePortId,
        disabled_sink: Option<SinkPortId>,
        width: Option<HostWidth>,
        padding_px: Option<u32>,
        rounded_fully: bool,
        background: Option<String>,
        background_sink: Option<SinkPortId>,
        active_background: Option<String>,
        outline_sink: Option<SinkPortId>,
        active_outline: Option<String>,
    },
    TimerSource {
        tick_port: SourcePortId,
        interval_ms: u32,
    },
    GenericElement {
        tag: String,
        text: Option<String>,
        properties: Vec<(String, Option<String>)>,
        styles: Vec<(String, Option<String>)>,
        input_value: Option<String>,
        checked: Option<bool>,
        event_bindings: Vec<HostElementEventBinding>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostTemplatedTextPart {
    Static(String),
    Sink(SinkPortId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostButtonLabel {
    Static(String),
    Sink(SinkPortId),
    Templated(Vec<HostTemplatedTextPart>),
}

impl HostButtonLabel {
    pub fn static_text(&self) -> Option<&str> {
        match self {
            Self::Static(text) => Some(text.as_str()),
            _ => None,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostViewNode {
    pub retained_key: RetainedNodeKey,
    pub kind: HostViewKind,
    pub children: Vec<HostViewNode>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostViewIr {
    pub root: Option<HostViewNode>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HostInput {
    Pulse {
        actor: ActorId,
        port: SourcePortId,
        value: KernelValue,
        seq: CausalSeq,
    },
    Mirror {
        actor: ActorId,
        cell: MirrorCellId,
        value: KernelValue,
        seq: CausalSeq,
    },
}

#[derive(Debug, Clone, Default, PartialEq)]
#[cfg(test)]
pub(crate) struct HostSnapshot {
    pub inputs: Vec<HostInput>,
}

#[cfg(test)]
impl HostSnapshot {
    #[must_use]
    pub(crate) fn new(inputs: Vec<HostInput>) -> Self {
        Self { inputs }
    }
}

pub(crate) fn enqueue_host_pulse(
    runtime: &mut RuntimeCore,
    actor: ActorId,
    port: SourcePortId,
    value: KernelValue,
    seq: CausalSeq,
) -> bool {
    runtime.push_message(actor, Msg::SourcePulse { port, value, seq })
}

pub(crate) fn enqueue_host_mirror(
    runtime: &mut RuntimeCore,
    actor: ActorId,
    cell: MirrorCellId,
    value: KernelValue,
    seq: CausalSeq,
) -> bool {
    runtime.push_message(actor, Msg::MirrorWrite { cell, value, seq })
}

/// Enqueue one coherent host snapshot into the runtime.
///
/// The bridge is responsible for collecting all pulse inputs and mirrored host
/// state writes derived from one host event before calling this function.
/// Ordering is preserved exactly as supplied by the bridge's lowering order.
#[cfg(test)]
pub(crate) fn enqueue_host_snapshot(runtime: &mut RuntimeCore, snapshot: &HostSnapshot) -> usize {
    enqueue_host_inputs(runtime, &snapshot.inputs)
}

pub(crate) fn enqueue_host_inputs(runtime: &mut RuntimeCore, inputs: &[HostInput]) -> usize {
    let mut queued = 0;

    for input in inputs {
        let accepted = match input {
            HostInput::Pulse {
                actor,
                port,
                value,
                seq,
            } => enqueue_host_pulse(runtime, *actor, *port, value.clone(), *seq),
            HostInput::Mirror {
                actor,
                cell,
                value,
                seq,
            } => enqueue_host_mirror(runtime, *actor, *cell, value.clone(), *seq),
        };

        if accepted {
            queued += 1;
        }
    }

    queued
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{FunctionInstanceId, ViewSiteId};
    use crate::runtime::RuntimeCore;

    #[test]
    fn host_snapshot_preserves_batch_order_for_pulses_and_mirrors() {
        let mut runtime = RuntimeCore::new();
        let actor = runtime.alloc_actor();

        let snapshot = HostSnapshot::new(vec![
            HostInput::Mirror {
                actor,
                cell: MirrorCellId(7),
                value: KernelValue::from("draft"),
                seq: CausalSeq::new(10, 0),
            },
            HostInput::Pulse {
                actor,
                port: SourcePortId(3),
                value: KernelValue::from("Enter"),
                seq: CausalSeq::new(10, 1),
            },
        ]);

        assert_eq!(enqueue_host_snapshot(&mut runtime, &snapshot), 2);

        let messages = runtime.queued_messages_for_actor(actor);
        assert_eq!(
            messages,
            vec![
                Msg::MirrorWrite {
                    cell: MirrorCellId(7),
                    value: KernelValue::from("draft"),
                    seq: CausalSeq::new(10, 0),
                },
                Msg::SourcePulse {
                    port: SourcePortId(3),
                    value: KernelValue::from("Enter"),
                    seq: CausalSeq::new(10, 1),
                },
            ]
        );
        assert_eq!(runtime.ready_actor_ids(), vec![actor]);
    }

    #[test]
    fn retained_node_identity_includes_view_site_function_instance_and_mapped_item() {
        let left = RetainedNodeKey {
            view_site: ViewSiteId(1),
            function_instance: Some(FunctionInstanceId(10)),
            mapped_item_identity: Some(20),
        };
        let same = RetainedNodeKey {
            view_site: ViewSiteId(1),
            function_instance: Some(FunctionInstanceId(10)),
            mapped_item_identity: Some(20),
        };
        let different_function = RetainedNodeKey {
            view_site: ViewSiteId(1),
            function_instance: Some(FunctionInstanceId(11)),
            mapped_item_identity: Some(20),
        };
        let different_item = RetainedNodeKey {
            view_site: ViewSiteId(1),
            function_instance: Some(FunctionInstanceId(10)),
            mapped_item_identity: Some(21),
        };

        assert_eq!(left, same);
        assert_ne!(left, different_function);
        assert_ne!(left, different_item);
    }

    #[test]
    fn host_view_ir_is_passive_retained_structure() {
        let retained = HostViewIr {
            root: Some(HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(2),
                    function_instance: Some(FunctionInstanceId(4)),
                    mapped_item_identity: Some(8),
                },
                kind: HostViewKind::Label {
                    sink: SinkPortId(5),
                },
                children: Vec::new(),
            }),
        };

        let node = retained.root.as_ref().expect("retained root");
        match node.kind {
            HostViewKind::Label { sink } => assert_eq!(sink, SinkPortId(5)),
            _ => panic!("expected label node"),
        }
        assert_eq!(node.retained_key.view_site, ViewSiteId(2));
    }

    #[test]
    fn snapshot_ignores_inputs_for_stale_actor_ids() {
        let mut runtime = RuntimeCore::new();
        let actor = runtime.alloc_actor();
        assert!(runtime.remove_actor(actor));
        let replacement = runtime.alloc_actor();
        assert_ne!(actor, replacement);

        let snapshot = HostSnapshot::new(vec![HostInput::Pulse {
            actor,
            port: SourcePortId(1),
            value: KernelValue::from("click"),
            seq: CausalSeq::new(1, 0),
        }]);

        assert_eq!(enqueue_host_snapshot(&mut runtime, &snapshot), 0);
        assert!(runtime.queued_messages_for_actor(replacement).is_empty());
    }
}
