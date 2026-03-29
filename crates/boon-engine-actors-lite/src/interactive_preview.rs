use crate::retained_ui_state::RetainedUiState;
use boon::platform::browser::kernel::KernelValue;
use boon::zoon::*;
use boon_renderer_zoon::FakeRenderState;
use boon_renderer_zoon::{RenderInteractionHandlers, render_retained_snapshot_signal};
use boon_scene::{
    EventPortId, NodeId, RenderRoot, UiEventBatch, UiEventKind, UiFactBatch, UiFactKind, UiNode,
    UiNodeKind,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::{JsCast, closure::Closure};

#[derive(Debug)]
pub(crate) struct InteractivePreviewState<Action, FactTarget> {
    retained_ui: RetainedUiState,
    event_bindings: HashMap<EventPortId, (crate::ir::SourcePortId, Action)>,
    fact_targets: HashMap<NodeId, FactTarget>,
}

impl<Action, FactTarget> Default for InteractivePreviewState<Action, FactTarget> {
    fn default() -> Self {
        Self {
            retained_ui: RetainedUiState::default(),
            event_bindings: HashMap::new(),
            fact_targets: HashMap::new(),
        }
    }
}

impl<Action, FactTarget> InteractivePreviewState<Action, FactTarget>
where
    Action: Clone,
    FactTarget: Clone,
{
    pub(crate) fn clear_bindings(&mut self) {
        self.event_bindings.clear();
        self.fact_targets.clear();
    }

    #[must_use]
    pub(crate) fn finalize_render(
        &self,
        root: UiNode,
        ops: Vec<boon_scene::RenderOp>,
    ) -> (RenderRoot, FakeRenderState) {
        self.retained_ui.finalize_render(root, ops)
    }

    pub(crate) fn bind_fact_target(&mut self, node_id: NodeId, target: FactTarget) {
        self.fact_targets.insert(node_id, target);
    }

    pub(crate) fn attach_port(
        &mut self,
        ops: &mut Vec<boon_scene::RenderOp>,
        node_id: NodeId,
        source_port: crate::ir::SourcePortId,
        kind: UiEventKind,
        action: Action,
    ) {
        let event_port = self
            .retained_ui
            .attach_port(ops, node_id, source_port, kind);
        self.event_bindings
            .insert(event_port, (source_port, action));
    }

    #[must_use]
    pub(crate) fn source_port_for_event_port(
        &self,
        event_port: EventPortId,
    ) -> Option<crate::ir::SourcePortId> {
        self.event_bindings
            .get(&event_port)
            .map(|(source_port, _action)| *source_port)
    }

    #[must_use]
    pub(crate) fn element_node(
        &mut self,
        retained_key: crate::ir::RetainedNodeKey,
        tag: &str,
        text: Option<String>,
        children: Vec<UiNode>,
    ) -> UiNode {
        self.retained_ui
            .element_node(retained_key, tag, text, children)
    }

    #[must_use]
    pub(crate) fn action_for_port(&self, source_port: crate::ir::SourcePortId) -> Option<Action> {
        self.event_bindings.values().find_map(|(port, action)| {
            if *port == source_port {
                Some(action.clone())
            } else {
                None
            }
        })
    }

    #[must_use]
    pub(crate) fn fact_target_for_node(&self, node_id: NodeId) -> Option<FactTarget> {
        self.fact_targets.get(&node_id).cloned()
    }
}

#[must_use]
pub(crate) fn preview_text_from_root(root: &RenderRoot) -> String {
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

    let mut text = String::new();
    if let RenderRoot::UiTree(root) = root {
        collect(root, &mut text);
    }
    text
}

const UI_EVENT_SEPARATOR: char = '\u{1E}';

#[must_use]
pub(crate) fn encode_ui_event(kind: UiEventKind, payload: Option<&str>) -> KernelValue {
    let kind = match kind {
        UiEventKind::Click => "click",
        UiEventKind::DoubleClick => "double_click",
        UiEventKind::Input => "input",
        UiEventKind::Change => "change",
        UiEventKind::KeyDown => "key_down",
        UiEventKind::Blur => "blur",
        UiEventKind::Focus => "focus",
        UiEventKind::Custom(name) => {
            return KernelValue::from(format!(
                "custom:{name}{UI_EVENT_SEPARATOR}{}",
                payload.unwrap_or_default()
            ));
        }
    };
    KernelValue::from(format!(
        "{kind}{UI_EVENT_SEPARATOR}{}",
        payload.unwrap_or_default()
    ))
}

#[must_use]
pub(crate) fn decode_ui_event(value: &KernelValue) -> Option<(UiEventKind, Option<String>)> {
    let text = match value {
        KernelValue::Text(text) | KernelValue::Tag(text) => text.as_str(),
        _ => return None,
    };
    let (kind, payload) = text.split_once(UI_EVENT_SEPARATOR)?;
    let kind = match kind {
        "click" => UiEventKind::Click,
        "double_click" => UiEventKind::DoubleClick,
        "input" => UiEventKind::Input,
        "change" => UiEventKind::Change,
        "key_down" => UiEventKind::KeyDown,
        "blur" => UiEventKind::Blur,
        "focus" => UiEventKind::Focus,
        custom if custom.starts_with("custom:") => {
            UiEventKind::Custom(custom["custom:".len()..].to_string())
        }
        _ => return None,
    };
    let payload = if payload.is_empty() {
        None
    } else {
        Some(payload.to_string())
    };
    Some((kind, payload))
}

#[must_use]
pub(crate) fn encode_ui_fact(kind: &UiFactKind) -> Option<KernelValue> {
    match kind {
        UiFactKind::DraftText(text) => Some(KernelValue::from(format!(
            "draft{UI_EVENT_SEPARATOR}{text}"
        ))),
        UiFactKind::Focused(focused) => Some(KernelValue::from(format!(
            "focused{UI_EVENT_SEPARATOR}{focused}"
        ))),
        UiFactKind::Hovered(hovered) => Some(KernelValue::from(format!(
            "hovered{UI_EVENT_SEPARATOR}{hovered}"
        ))),
        _ => None,
    }
}

#[must_use]
pub(crate) fn decode_ui_fact(value: &KernelValue) -> Option<UiFactKind> {
    let text = match value {
        KernelValue::Text(text) | KernelValue::Tag(text) => text.as_str(),
        _ => return None,
    };
    let (kind, payload) = text.split_once(UI_EVENT_SEPARATOR)?;
    match kind {
        "draft" => Some(UiFactKind::DraftText(payload.to_string())),
        "focused" => Some(UiFactKind::Focused(payload == "true")),
        "hovered" => Some(UiFactKind::Hovered(payload == "true")),
        _ => None,
    }
}

pub(crate) trait InteractivePreview {
    fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool;
    fn dispatch_ui_facts(&mut self, batch: UiFactBatch) -> bool;
    fn render_snapshot(&mut self) -> (RenderRoot, FakeRenderState);
}

pub(crate) fn render_interactive_preview<Preview>(preview: Preview) -> impl Element
where
    Preview: InteractivePreview + 'static,
{
    let preview = Rc::new(RefCell::new(preview));
    let snapshot = Mutable::new(preview.borrow_mut().render_snapshot());
    let rerender_pending = Rc::new(Cell::new(false));

    let schedule_rerender = {
        let snapshot = snapshot.clone();
        let rerender_pending = rerender_pending.clone();
        let preview = preview.clone();
        move || {
            if rerender_pending.replace(true) {
                return;
            }
            #[cfg(target_arch = "wasm32")]
            {
                if let Some(window) = web_sys::window() {
                    let snapshot = snapshot.clone();
                    let preview = preview.clone();
                    let rerender_pending = rerender_pending.clone();
                    let callback = Closure::once(move || {
                        rerender_pending.set(false);
                        snapshot.set(preview.borrow_mut().render_snapshot());
                    });
                    let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                        callback.as_ref().unchecked_ref(),
                        0,
                    );
                    callback.forget();
                } else {
                    rerender_pending.set(false);
                    snapshot.set(preview.borrow_mut().render_snapshot());
                }
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                rerender_pending.set(false);
                snapshot.set(preview.borrow_mut().render_snapshot());
            }
        }
    };

    let handlers = RenderInteractionHandlers::new(
        {
            let preview = preview.clone();
            let schedule_rerender = schedule_rerender.clone();
            move |batch: UiEventBatch| {
                if preview.borrow_mut().dispatch_ui_events(batch) {
                    schedule_rerender();
                }
            }
        },
        {
            let preview = preview.clone();
            let schedule_rerender = schedule_rerender.clone();
            move |batch: UiFactBatch| {
                if preview.borrow_mut().dispatch_ui_facts(batch) {
                    schedule_rerender();
                }
            }
        },
    );

    render_retained_snapshot_signal(snapshot.signal_cloned(), handlers)
}

#[cfg(test)]
mod codec_tests {
    use super::*;

    #[test]
    fn ui_event_codec_round_trips_key_down_payload() {
        let encoded = encode_ui_event(UiEventKind::KeyDown, Some("Enter"));
        let decoded = decode_ui_event(&encoded);
        assert_eq!(
            decoded,
            Some((UiEventKind::KeyDown, Some("Enter".to_string())))
        );
    }

    #[test]
    fn ui_fact_codec_round_trips_common_fact_variants() {
        assert_eq!(
            decode_ui_fact(&encode_ui_fact(&UiFactKind::DraftText("x".to_string())).unwrap()),
            Some(UiFactKind::DraftText("x".to_string()))
        );
        assert_eq!(
            decode_ui_fact(&encode_ui_fact(&UiFactKind::Focused(true)).unwrap()),
            Some(UiFactKind::Focused(true))
        );
        assert_eq!(
            decode_ui_fact(&encode_ui_fact(&UiFactKind::Hovered(false)).unwrap()),
            Some(UiFactKind::Hovered(false))
        );
    }
}
