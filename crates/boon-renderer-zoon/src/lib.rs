pub use boon_scene::{
    EventPortId, NodeId, RenderDiffBatch, RenderNode, RenderOp, RenderRoot, RenderRootHandle,
    RenderSurface, SceneDiff, SceneNode, SceneNodeKind, UiEvent, UiEventBatch, UiEventKind, UiFact,
    UiFactBatch, UiFactKind, UiNode, UiNodeKind,
};
pub use zoon;

use std::collections::HashMap;
use std::cell::RefCell;
use std::rc::Rc;

use ulid::Ulid;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use zoon::js_sys::Reflect;
use zoon::{ElementUnchecked, RawEl, Unify};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderMode {
    UiTree,
    SceneFallback,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RendererCapabilities {
    pub supports_ui_tree: bool,
    pub supports_scene_fallback: bool,
}

impl RendererCapabilities {
    #[must_use]
    pub const fn zoon() -> Self {
        Self {
            supports_ui_tree: true,
            supports_scene_fallback: true,
        }
    }
}

#[must_use]
pub const fn render_mode_for_surface(surface: RenderSurface) -> RenderMode {
    match surface {
        RenderSurface::Document => RenderMode::UiTree,
        RenderSurface::Scene => RenderMode::SceneFallback,
    }
}

#[must_use]
pub fn fallback_text(text: impl zoon::IntoCowStr<'static>) -> zoon::RawElOrText {
    zoon::Text::new(text).unify()
}

#[must_use]
pub fn empty_text() -> zoon::RawElOrText {
    fallback_text("")
}

#[must_use]
pub fn missing_document_root() -> zoon::RawElOrText {
    fallback_text("No document root")
}

#[must_use]
pub fn with_render_root<T>(
    root: Option<RenderRootHandle<T>>,
    render: impl FnOnce(RenderRootHandle<T>) -> zoon::RawElOrText,
) -> zoon::RawElOrText {
    match root {
        Some(root) => render(root),
        None => missing_document_root(),
    }
}

#[must_use]
pub fn unknown_placeholder() -> zoon::RawElOrText {
    fallback_text("?")
}

#[must_use]
pub fn custom_call_placeholder(path: &[String]) -> zoon::RawElOrText {
    fallback_text(format!("[{}]", path.join("/")))
}

#[must_use]
pub fn select_placeholder(selected: &str) -> zoon::RawElOrText {
    fallback_text(format!("[select: {}]", selected))
}

#[must_use]
pub fn slider_placeholder(value: f64) -> zoon::RawElOrText {
    fallback_text(format!("[slider: {}]", value))
}

#[must_use]
pub fn svg_canvas_placeholder() -> zoon::RawElOrText {
    fallback_text("[SVG canvas]")
}

#[must_use]
pub fn tagged_placeholder(tag: &str) -> zoon::RawElOrText {
    fallback_text(format!("{}[...]", tag))
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FakeRenderState {
    root: Option<RenderRoot>,
    properties: HashMap<NodeId, HashMap<String, Option<String>>>,
    styles: HashMap<NodeId, HashMap<String, Option<String>>>,
    class_flags: HashMap<NodeId, HashMap<String, bool>>,
    event_ports: HashMap<NodeId, Vec<(EventPortId, UiEventKind)>>,
    input_values: HashMap<NodeId, String>,
    checked_values: HashMap<NodeId, bool>,
    selected_indices: HashMap<NodeId, Option<usize>>,
    scene_params: HashMap<String, String>,
}

#[derive(Clone)]
pub struct RenderInteractionHandlers {
    on_ui_events: Rc<dyn Fn(UiEventBatch)>,
    on_ui_facts: Rc<dyn Fn(UiFactBatch)>,
}

impl Default for RenderInteractionHandlers {
    fn default() -> Self {
        Self {
            on_ui_events: Rc::new(|_| {}),
            on_ui_facts: Rc::new(|_| {}),
        }
    }
}

impl RenderInteractionHandlers {
    #[must_use]
    pub fn new(
        on_ui_events: impl Fn(UiEventBatch) + 'static,
        on_ui_facts: impl Fn(UiFactBatch) + 'static,
    ) -> Self {
        Self {
            on_ui_events: Rc::new(on_ui_events),
            on_ui_facts: Rc::new(on_ui_facts),
        }
    }

    fn emit_event(&self, event: UiEvent) {
        (self.on_ui_events)(UiEventBatch {
            events: vec![event],
        });
    }

    fn emit_fact(&self, fact: UiFact) {
        (self.on_ui_facts)(UiFactBatch { facts: vec![fact] });
    }
}

thread_local! {
    static AUTOMATION_HANDLERS: RefCell<RenderInteractionHandlers> = RefCell::new(RenderInteractionHandlers::default());
    static AUTOMATION_HOOKS_INSTALLED: RefCell<bool> = const { RefCell::new(false) };
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenderApplyError {
    MissingRoot,
    ParentNotFound(NodeId),
    NodeNotFound(NodeId),
    NodeTypeMismatch { parent: NodeId },
    CannotMoveRoot(NodeId),
}

impl FakeRenderState {
    pub fn apply_batch(&mut self, batch: &RenderDiffBatch) -> Result<(), RenderApplyError> {
        for op in &batch.ops {
            self.apply_op(op)?;
        }
        Ok(())
    }

    #[must_use]
    pub fn root(&self) -> Option<&RenderRoot> {
        self.root.as_ref()
    }

    fn properties_for(&self, id: NodeId) -> impl Iterator<Item = (&String, &Option<String>)> {
        self.properties
            .get(&id)
            .into_iter()
            .flat_map(|properties| properties.iter())
    }

    fn styles_for(&self, id: NodeId) -> impl Iterator<Item = (&String, &Option<String>)> {
        self.styles
            .get(&id)
            .into_iter()
            .flat_map(|styles| styles.iter())
    }

    fn enabled_classes_for(&self, id: NodeId) -> impl Iterator<Item = &String> {
        self.class_flags
            .get(&id)
            .into_iter()
            .flat_map(|classes| classes.iter())
            .filter_map(|(class_name, enabled)| enabled.then_some(class_name))
    }

    fn event_ports_for(&self, id: NodeId) -> Vec<(EventPortId, UiEventKind)> {
        self.event_ports.get(&id).cloned().unwrap_or_default()
    }

    fn input_value_for(&self, id: NodeId) -> Option<&String> {
        self.input_values.get(&id)
    }

    fn apply_op(&mut self, op: &RenderOp) -> Result<(), RenderApplyError> {
        match op {
            RenderOp::ReplaceRoot(root) => self.root = Some(root.clone()),
            RenderOp::InsertChild {
                parent,
                index,
                node,
            } => self.insert_child(*parent, *index, node.clone())?,
            RenderOp::RemoveNode { id } => {
                self.remove_node(*id)?;
            }
            RenderOp::MoveChild { parent, id, index } => {
                self.move_child(*parent, *id, *index)?;
            }
            RenderOp::SetText { id, text } => self.set_text(*id, text.clone())?,
            RenderOp::SetProperty { id, name, value } => {
                self.properties
                    .entry(*id)
                    .or_default()
                    .insert(name.clone(), value.clone());
            }
            RenderOp::SetStyle { id, name, value } => {
                self.styles
                    .entry(*id)
                    .or_default()
                    .insert(name.clone(), value.clone());
            }
            RenderOp::SetClassFlag {
                id,
                class_name,
                enabled,
            } => {
                self.class_flags
                    .entry(*id)
                    .or_default()
                    .insert(class_name.clone(), *enabled);
            }
            RenderOp::AttachEventPort { id, port, kind } => {
                let entry = self.event_ports.entry(*id).or_default();
                if let Some(existing) = entry
                    .iter_mut()
                    .find(|(existing_port, _)| *existing_port == *port)
                {
                    existing.1 = kind.clone();
                } else {
                    entry.push((*port, kind.clone()));
                }
            }
            RenderOp::DetachEventPort { id, port } => {
                if let Some(ports) = self.event_ports.get_mut(id) {
                    ports.retain(|(existing_port, _)| existing_port != port);
                }
            }
            RenderOp::SetInputValue { id, value } => {
                self.input_values.insert(*id, value.clone());
            }
            RenderOp::SetChecked { id, checked } => {
                self.checked_values.insert(*id, *checked);
            }
            RenderOp::SetSelectedIndex { id, index } => {
                self.selected_indices.insert(*id, *index);
            }
            RenderOp::UpdateSceneParam { name, value } => {
                self.scene_params.insert(name.clone(), value.clone());
            }
        }
        Ok(())
    }

    fn insert_child(
        &mut self,
        parent: NodeId,
        index: usize,
        node: RenderNode,
    ) -> Result<(), RenderApplyError> {
        match (self.root.as_mut(), node) {
            (Some(RenderRoot::UiTree(root)), RenderNode::Ui(node)) => {
                let parent_node = find_ui_node_mut(root, parent)
                    .ok_or(RenderApplyError::ParentNotFound(parent))?;
                let insert_at = index.min(parent_node.children.len());
                parent_node.children.insert(insert_at, node);
                Ok(())
            }
            (Some(RenderRoot::SceneGraph(root)), RenderNode::Scene(node)) => {
                let parent_node = find_scene_node_mut(root, parent)
                    .ok_or(RenderApplyError::ParentNotFound(parent))?;
                let insert_at = index.min(parent_node.children.len());
                parent_node.children.insert(insert_at, node);
                Ok(())
            }
            (Some(_), _) => Err(RenderApplyError::NodeTypeMismatch { parent }),
            (None, _) => Err(RenderApplyError::MissingRoot),
        }
    }

    fn remove_node(&mut self, id: NodeId) -> Result<(), RenderApplyError> {
        match self.root.as_mut() {
            Some(RenderRoot::UiTree(root)) => {
                if root.id == id {
                    self.root = None;
                    return Ok(());
                }
                if detach_ui_node(root, id).is_some() {
                    return Ok(());
                }
                Err(RenderApplyError::NodeNotFound(id))
            }
            Some(RenderRoot::SceneGraph(root)) => {
                if root.id == id {
                    self.root = None;
                    return Ok(());
                }
                if detach_scene_node(root, id).is_some() {
                    return Ok(());
                }
                Err(RenderApplyError::NodeNotFound(id))
            }
            None => Err(RenderApplyError::MissingRoot),
        }
    }

    fn move_child(
        &mut self,
        parent: NodeId,
        id: NodeId,
        index: usize,
    ) -> Result<(), RenderApplyError> {
        match self.root.as_mut() {
            Some(RenderRoot::UiTree(root)) => {
                if root.id == id {
                    return Err(RenderApplyError::CannotMoveRoot(id));
                }
                let node = detach_ui_node(root, id).ok_or(RenderApplyError::NodeNotFound(id))?;
                let parent_node = find_ui_node_mut(root, parent)
                    .ok_or(RenderApplyError::ParentNotFound(parent))?;
                let insert_at = index.min(parent_node.children.len());
                parent_node.children.insert(insert_at, node);
                Ok(())
            }
            Some(RenderRoot::SceneGraph(root)) => {
                if root.id == id {
                    return Err(RenderApplyError::CannotMoveRoot(id));
                }
                let node = detach_scene_node(root, id).ok_or(RenderApplyError::NodeNotFound(id))?;
                let parent_node = find_scene_node_mut(root, parent)
                    .ok_or(RenderApplyError::ParentNotFound(parent))?;
                let insert_at = index.min(parent_node.children.len());
                parent_node.children.insert(insert_at, node);
                Ok(())
            }
            None => Err(RenderApplyError::MissingRoot),
        }
    }

    fn set_text(&mut self, id: NodeId, text: String) -> Result<(), RenderApplyError> {
        match self.root.as_mut() {
            Some(RenderRoot::UiTree(root)) => {
                let node = find_ui_node_mut(root, id).ok_or(RenderApplyError::NodeNotFound(id))?;
                match &mut node.kind {
                    UiNodeKind::Element { text: slot, .. } => *slot = Some(text),
                    UiNodeKind::Text { text: slot } => *slot = text,
                }
                Ok(())
            }
            Some(RenderRoot::SceneGraph(root)) => {
                let node =
                    find_scene_node_mut(root, id).ok_or(RenderApplyError::NodeNotFound(id))?;
                match &mut node.kind {
                    SceneNodeKind::Label { text: slot } => *slot = text,
                    SceneNodeKind::Primitive { primitive } => *primitive = text,
                    SceneNodeKind::Group => {}
                }
                Ok(())
            }
            None => Err(RenderApplyError::MissingRoot),
        }
    }
}

#[must_use]
pub fn render_fake_state(state: &FakeRenderState) -> zoon::RawElOrText {
    render_fake_state_with_handlers(state, &RenderInteractionHandlers::default())
}

#[must_use]
pub fn render_fake_state_with_handlers(
    state: &FakeRenderState,
    handlers: &RenderInteractionHandlers,
) -> zoon::RawElOrText {
    install_automation_hooks(handlers.clone());
    match state.root() {
        Some(root) => render_snapshot_root_with_handlers(root, state, handlers),
        None => empty_text(),
    }
}

#[must_use]
pub fn render_snapshot_root(root: &RenderRoot) -> zoon::RawElOrText {
    render_snapshot_root_with_handlers(
        root,
        &FakeRenderState::default(),
        &RenderInteractionHandlers::default(),
    )
}

#[must_use]
pub fn render_snapshot_root_with_handlers(
    root: &RenderRoot,
    state: &FakeRenderState,
    handlers: &RenderInteractionHandlers,
) -> zoon::RawElOrText {
    install_automation_hooks(handlers.clone());
    match root {
        RenderRoot::UiTree(node) => render_ui_node(node, state, handlers),
        RenderRoot::SceneGraph(node) => render_scene_node(node),
    }
}

fn install_automation_hooks(handlers: RenderInteractionHandlers) {
    AUTOMATION_HANDLERS.with(|slot| {
        *slot.borrow_mut() = handlers;
    });

    let Some(window) = web_sys::window() else {
        return;
    };
    AUTOMATION_HOOKS_INSTALLED.with(|flag| {
        if *flag.borrow() {
            return;
        }
        *flag.borrow_mut() = true;

        let event_window = window.clone();
        let dispatch_event = Closure::<dyn FnMut(String, String, wasm_bindgen::JsValue)>::wrap(
            Box::new(move |port_id, kind, payload| {
                let Some(kind) = parse_ui_event_kind(&kind) else {
                    return;
                };
                let Ok(port_ulid) = Ulid::from_string(&port_id) else {
                    return;
                };
                let payload = payload.as_string();
                AUTOMATION_HANDLERS.with(|slot| {
                    slot.borrow().emit_event(UiEvent {
                        target: EventPortId(port_ulid),
                        kind,
                        payload,
                    });
                });
            }),
        );
        let _ = Reflect::set(
            event_window.as_ref(),
            &wasm_bindgen::JsValue::from_str("__boonDispatchUiEvent"),
            dispatch_event.as_ref().unchecked_ref(),
        );
        dispatch_event.forget();

        let fact_window = window.clone();
        let dispatch_fact = Closure::<dyn FnMut(String, String, wasm_bindgen::JsValue)>::wrap(
            Box::new(move |node_id, kind, value| {
                let Some(kind) = parse_ui_fact_kind(&kind, &value) else {
                    return;
                };
                let Ok(node_ulid) = Ulid::from_string(&node_id) else {
                    return;
                };
                AUTOMATION_HANDLERS.with(|slot| {
                    slot.borrow().emit_fact(UiFact {
                        id: NodeId(node_ulid),
                        kind,
                    });
                });
            }),
        );
        let _ = Reflect::set(
            fact_window.as_ref(),
            &wasm_bindgen::JsValue::from_str("__boonDispatchUiFact"),
            dispatch_fact.as_ref().unchecked_ref(),
        );
        dispatch_fact.forget();
    });
}

fn parse_ui_event_kind(kind: &str) -> Option<UiEventKind> {
    match kind {
        "Click" => Some(UiEventKind::Click),
        "DoubleClick" => Some(UiEventKind::DoubleClick),
        "Input" => Some(UiEventKind::Input),
        "Change" => Some(UiEventKind::Change),
        "KeyDown" => Some(UiEventKind::KeyDown),
        "Blur" => Some(UiEventKind::Blur),
        "Focus" => Some(UiEventKind::Focus),
        _ => None,
    }
}

fn parse_ui_fact_kind(kind: &str, value: &wasm_bindgen::JsValue) -> Option<UiFactKind> {
    match kind {
        "DraftText" => Some(UiFactKind::DraftText(value.as_string().unwrap_or_default())),
        "Focused" => Some(UiFactKind::Focused(value.as_bool().unwrap_or(false))),
        "Hovered" => Some(UiFactKind::Hovered(value.as_bool().unwrap_or(false))),
        _ => None,
    }
}

fn render_ui_node(
    node: &UiNode,
    state: &FakeRenderState,
    handlers: &RenderInteractionHandlers,
) -> zoon::RawElOrText {
    match &node.kind {
        UiNodeKind::Text { text } => zoon::Text::new(text.clone()).unify(),
        UiNodeKind::Element { tag, text, .. } => {
            let children: Vec<zoon::RawElOrText> = node
                .children
                .iter()
                .map(|child| render_ui_node(child, state, handlers))
                .collect();
            let node_id_value = node.id;
            let node_id = node.id.0.to_string();
            let mut should_focus_after_insert = false;
            let mut el = raw_html_el_for_tag(tag)
                .attr("data-boon-tag", tag)
                .attr("data-boon-node-id", &node_id)
                .event_handler({
                    let handlers = handlers.clone();
                    move |_: zoon::events::MouseEnter| {
                        handlers.emit_fact(UiFact {
                            id: node_id_value,
                            kind: UiFactKind::Hovered(true),
                        });
                    }
                })
                .event_handler({
                    let handlers = handlers.clone();
                    move |_: zoon::events::MouseLeave| {
                        handlers.emit_fact(UiFact {
                            id: node_id_value,
                            kind: UiFactKind::Hovered(false),
                        });
                    }
                })
                .event_handler({
                    let handlers = handlers.clone();
                    move |_: zoon::events::Focus| {
                        handlers.emit_fact(UiFact {
                            id: node_id_value,
                            kind: UiFactKind::Focused(true),
                        });
                    }
                })
                .event_handler({
                    let handlers = handlers.clone();
                    move |_: zoon::events::Blur| {
                        handlers.emit_fact(UiFact {
                            id: node_id_value,
                            kind: UiFactKind::Focused(false),
                        });
                    }
                });
            for (name, value) in state.properties_for(node_id_value) {
                if let Some(value) = value {
                    if tag == "input" && name == "autofocus" && value == "true" {
                        should_focus_after_insert = true;
                    }
                    el = el.attr(name, value);
                }
            }
            for (name, value) in state.styles_for(node_id_value) {
                if let Some(value) = value {
                    el = el.style(name, value);
                }
            }
            let enabled_classes: Vec<&str> = state
                .enabled_classes_for(node_id_value)
                .map(String::as_str)
                .collect();
            if !enabled_classes.is_empty() {
                let class_names = enabled_classes.join(" ");
                el = el.attr("class", &class_names);
            }
            if tag == "input" {
                if let Some(value) = state.input_value_for(node_id_value) {
                    el = el.attr("value", value);
                }
                if should_focus_after_insert {
                    el = el
                        .attr("data-boon-focused", "true")
                        .attr("focused", "true");
                }
            }
            for (port, kind) in state.event_ports_for(node_id_value) {
                el = el.attr(event_port_attr_name(&kind), &port.0.to_string());
                el = attach_ui_handler(el, handlers.clone(), node_id_value, port, kind);
            }
            if should_focus_after_insert {
                el = el.after_insert(|element| {
                    if let Some(input) = element.dyn_ref::<web_sys::HtmlInputElement>() {
                        let _ = input.focus();
                        let _ = input.select();
                    } else if let Some(html) = element.dyn_ref::<web_sys::HtmlElement>() {
                        let _ = html.focus();
                    }
                    if let Some(window) = web_sys::window() {
                        let delayed_element = element.clone();
                        let callback = wasm_bindgen::closure::Closure::once(move || {
                            if let Some(input) = delayed_element.dyn_ref::<web_sys::HtmlInputElement>() {
                                let _ = input.focus();
                                let _ = input.select();
                            } else if let Some(html) = delayed_element.dyn_ref::<web_sys::HtmlElement>() {
                                let _ = html.focus();
                            }
                        });
                        let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                            callback.as_ref().unchecked_ref(),
                            0,
                        );
                        callback.forget();
                    }
                });
            }
            if let Some(text) = text {
                el = el.child(zoon::Text::new(text.clone()).unify());
            }
            el.children(children).into_raw_unchecked()
        }
    }
}

fn raw_html_el_for_tag(tag: &str) -> zoon::RawHtmlEl<web_sys::HtmlElement> {
    match tag {
        "a" => zoon::RawHtmlEl::new("a"),
        "button" => zoon::RawHtmlEl::new("button"),
        "div" => zoon::RawHtmlEl::new("div"),
        "footer" => zoon::RawHtmlEl::new("footer"),
        "h1" => zoon::RawHtmlEl::new("h1"),
        "header" => zoon::RawHtmlEl::new("header"),
        "input" => zoon::RawHtmlEl::new("input"),
        "label" => zoon::RawHtmlEl::new("label"),
        "p" => zoon::RawHtmlEl::new("p"),
        "section" => zoon::RawHtmlEl::new("section"),
        "span" => zoon::RawHtmlEl::new("span"),
        _ => zoon::RawHtmlEl::new("div"),
    }
}

fn render_scene_node(node: &SceneNode) -> zoon::RawElOrText {
    let label = match &node.kind {
        SceneNodeKind::Group => "[scene-group]".to_string(),
        SceneNodeKind::Primitive { primitive } => format!("[primitive: {primitive}]"),
        SceneNodeKind::Label { text } => text.clone(),
    };
    let children: Vec<zoon::RawElOrText> = node.children.iter().map(render_scene_node).collect();
    let node_id = node.id.0.to_string();
    zoon::RawHtmlEl::new("div")
        .attr("data-boon-scene-node-id", &node_id)
        .child(zoon::Text::new(label).unify())
        .children(children)
        .into_raw_unchecked()
}

fn attach_ui_handler(
    raw_el: zoon::RawHtmlEl<web_sys::HtmlElement>,
    handlers: RenderInteractionHandlers,
    node_id: NodeId,
    port: EventPortId,
    kind: UiEventKind,
) -> zoon::RawHtmlEl<web_sys::HtmlElement> {
    match kind {
        UiEventKind::Click => raw_el.event_handler(move |_: zoon::events::Click| {
            handlers.emit_event(UiEvent {
                target: port,
                kind: UiEventKind::Click,
                payload: None,
            });
        }),
        UiEventKind::DoubleClick => raw_el.event_handler(move |_: zoon::events::DoubleClick| {
            handlers.emit_event(UiEvent {
                target: port,
                kind: UiEventKind::DoubleClick,
                payload: None,
            });
        }),
        UiEventKind::Input => raw_el.event_handler(move |event: zoon::events::Input| {
            let payload = input_event_value(event.target());
            if let Some(text) = payload.clone() {
                handlers.emit_fact(UiFact {
                    id: node_id,
                    kind: UiFactKind::DraftText(text),
                });
            }
            handlers.emit_event(UiEvent {
                target: port,
                kind: UiEventKind::Input,
                payload,
            });
        }),
        UiEventKind::Change => raw_el.event_handler(move |event: zoon::events::Change| {
            let payload = input_event_value(event.target());
            if let Some(text) = payload.clone() {
                handlers.emit_fact(UiFact {
                    id: node_id,
                    kind: UiFactKind::DraftText(text),
                });
            }
            handlers.emit_event(UiEvent {
                target: port,
                kind: UiEventKind::Change,
                payload,
            });
        }),
        UiEventKind::Blur => raw_el.event_handler(move |_: zoon::events::Blur| {
            handlers.emit_event(UiEvent {
                target: port,
                kind: UiEventKind::Blur,
                payload: None,
            });
        }),
        UiEventKind::Focus => raw_el.event_handler(move |_: zoon::events::Focus| {
            handlers.emit_event(UiEvent {
                target: port,
                kind: UiEventKind::Focus,
                payload: None,
            });
        }),
        UiEventKind::KeyDown => raw_el.event_handler(move |event: zoon::events::KeyDown| {
            let payload = Some(encode_key_down_payload(
                &event.key(),
                input_event_value(event.target()).as_deref(),
            ));
            handlers.emit_event(UiEvent {
                target: port,
                kind: UiEventKind::KeyDown,
                payload,
            });
        }),
        UiEventKind::Custom(_) => raw_el,
    }
}

fn encode_key_down_payload(key: &str, current_text: Option<&str>) -> String {
    const KEYDOWN_TEXT_SEPARATOR: char = '\u{1F}';
    match current_text {
        Some(text) => format!("{key}{KEYDOWN_TEXT_SEPARATOR}{text}"),
        None => key.to_string(),
    }
}

fn event_port_attr_name(kind: &UiEventKind) -> &'static str {
    match kind {
        UiEventKind::Click => "data-boon-port-click",
        UiEventKind::DoubleClick => "data-boon-port-double-click",
        UiEventKind::Input => "data-boon-port-input",
        UiEventKind::Change => "data-boon-port-change",
        UiEventKind::KeyDown => "data-boon-port-key-down",
        UiEventKind::Blur => "data-boon-port-blur",
        UiEventKind::Focus => "data-boon-port-focus",
        UiEventKind::Custom(_) => "data-boon-port-custom",
    }
}

fn input_event_value(target: Option<web_sys::EventTarget>) -> Option<String> {
    let target = target?;
    target
        .dyn_ref::<web_sys::HtmlInputElement>()
        .map(|input| input.value())
        .or_else(|| {
            target
                .dyn_ref::<web_sys::HtmlTextAreaElement>()
                .map(|textarea| textarea.value())
        })
        .or_else(|| {
            target
                .dyn_ref::<web_sys::HtmlSelectElement>()
                .map(|select| select.value())
        })
}

fn find_ui_node_mut(node: &mut UiNode, id: NodeId) -> Option<&mut UiNode> {
    if node.id == id {
        return Some(node);
    }
    for child in &mut node.children {
        if let Some(found) = find_ui_node_mut(child, id) {
            return Some(found);
        }
    }
    None
}

fn detach_ui_node(node: &mut UiNode, id: NodeId) -> Option<UiNode> {
    if let Some(index) = node.children.iter().position(|child| child.id == id) {
        return Some(node.children.remove(index));
    }
    for child in &mut node.children {
        if let Some(found) = detach_ui_node(child, id) {
            return Some(found);
        }
    }
    None
}

fn find_scene_node_mut(node: &mut SceneNode, id: NodeId) -> Option<&mut SceneNode> {
    if node.id == id {
        return Some(node);
    }
    for child in &mut node.children {
        if let Some(found) = find_scene_node_mut(child, id) {
            return Some(found);
        }
    }
    None
}

fn detach_scene_node(node: &mut SceneNode, id: NodeId) -> Option<SceneNode> {
    if let Some(index) = node.children.iter().position(|child| child.id == id) {
        return Some(node.children.remove(index));
    }
    for child in &mut node.children {
        if let Some(found) = detach_scene_node(child, id) {
            return Some(found);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{
        EventPortId, FakeRenderState, NodeId, RenderDiffBatch, RenderMode, RenderNode, RenderOp,
        RenderRoot, RendererCapabilities, UiNode, UiNodeKind, render_mode_for_surface,
    };
    use boon_scene::RenderSurface;

    #[test]
    fn zoon_renderer_reports_expected_capabilities() {
        let capabilities = RendererCapabilities::zoon();

        assert!(capabilities.supports_ui_tree);
        assert!(capabilities.supports_scene_fallback);
    }

    #[test]
    fn render_surface_maps_to_expected_mode() {
        assert_eq!(
            render_mode_for_surface(RenderSurface::Document),
            RenderMode::UiTree
        );
        assert_eq!(
            render_mode_for_surface(RenderSurface::Scene),
            RenderMode::SceneFallback
        );
    }

    #[test]
    fn zoon_renderer_reexports_render_diff_types() {
        let batch = RenderDiffBatch {
            ops: vec![RenderOp::DetachEventPort {
                id: NodeId::new(),
                port: EventPortId::new(),
            }],
        };

        assert_eq!(batch.ops.len(), 1);
    }

    #[test]
    fn fake_renderer_applies_replace_root_and_insert_child() {
        let root = UiNode::new(UiNodeKind::Element {
            tag: "div".to_string(),
            text: Some("root".to_string()),
            event_ports: Vec::new(),
        });
        let child = UiNode::new(UiNodeKind::Text {
            text: "child".to_string(),
        });
        let root_id = root.id;
        let child_id = child.id;
        let mut state = FakeRenderState::default();

        state
            .apply_batch(&RenderDiffBatch {
                ops: vec![
                    RenderOp::ReplaceRoot(RenderRoot::UiTree(root)),
                    RenderOp::InsertChild {
                        parent: root_id,
                        index: 0,
                        node: RenderNode::Ui(child),
                    },
                ],
            })
            .expect("batch should apply");

        let Some(RenderRoot::UiTree(rendered_root)) = state.root() else {
            panic!("expected ui root");
        };
        assert_eq!(rendered_root.children.len(), 1);
        assert_eq!(rendered_root.children[0].id, child_id);
    }

    #[test]
    fn fake_renderer_can_move_and_remove_child() {
        let first = UiNode::new(UiNodeKind::Text {
            text: "first".to_string(),
        });
        let second = UiNode::new(UiNodeKind::Text {
            text: "second".to_string(),
        });
        let first_id = first.id;
        let second_id = second.id;
        let root = UiNode::new(UiNodeKind::Element {
            tag: "div".to_string(),
            text: None,
            event_ports: Vec::new(),
        })
        .with_children(vec![first, second]);
        let root_id = root.id;
        let mut state = FakeRenderState::default();
        state
            .apply_batch(&RenderDiffBatch {
                ops: vec![
                    RenderOp::ReplaceRoot(RenderRoot::UiTree(root)),
                    RenderOp::MoveChild {
                        parent: root_id,
                        id: second_id,
                        index: 0,
                    },
                    RenderOp::RemoveNode { id: first_id },
                ],
            })
            .expect("batch should apply");

        let Some(RenderRoot::UiTree(rendered_root)) = state.root() else {
            panic!("expected ui root");
        };
        assert_eq!(rendered_root.children.len(), 1);
        assert_eq!(rendered_root.children[0].id, second_id);
    }

    #[test]
    fn fake_renderer_updates_text_for_existing_node() {
        let child = UiNode::new(UiNodeKind::Text {
            text: "before".to_string(),
        });
        let child_id = child.id;
        let root = UiNode::new(UiNodeKind::Element {
            tag: "div".to_string(),
            text: None,
            event_ports: Vec::new(),
        })
        .with_children(vec![child]);
        let mut state = FakeRenderState::default();
        state
            .apply_batch(&RenderDiffBatch {
                ops: vec![
                    RenderOp::ReplaceRoot(RenderRoot::UiTree(root)),
                    RenderOp::SetText {
                        id: child_id,
                        text: "after".to_string(),
                    },
                ],
            })
            .expect("batch should apply");

        let Some(RenderRoot::UiTree(rendered_root)) = state.root() else {
            panic!("expected ui root");
        };
        let UiNodeKind::Text { text } = &rendered_root.children[0].kind else {
            panic!("expected text node");
        };
        assert_eq!(text, "after");
    }
}
