pub use boon_scene::{
    EventPortId, NodeId, RenderDiffBatch, RenderNode, RenderOp, RenderRoot, RenderRootHandle,
    RenderSurface, SceneDiff, SceneNode, SceneNodeKind, UiEvent, UiEventBatch, UiEventKind, UiFact,
    UiFactBatch, UiFactKind, UiNode, UiNodeKind,
};
pub use zoon;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use ulid::Ulid;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use zoon::js_sys::Reflect;
use zoon::{
    Element, ElementUnchecked, Mutable, RawEl, RawElWrapper, ReadOnlyMutableExtOption, Signal,
    SignalExtExt, Task, Unify,
};

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

    pub fn dispatch_event_batch(&self, batch: UiEventBatch) {
        (self.on_ui_events)(batch);
    }

    pub fn dispatch_fact_batch(&self, batch: UiFactBatch) {
        (self.on_ui_facts)(batch);
    }

    fn emit_event(&self, event: UiEvent) {
        self.dispatch_event_batch(UiEventBatch {
            events: vec![event],
        });
    }

    fn emit_fact(&self, fact: UiFact) {
        self.dispatch_fact_batch(UiFactBatch { facts: vec![fact] });
    }
}

thread_local! {
    static AUTOMATION_HANDLERS: RefCell<RenderInteractionHandlers> = RefCell::new(RenderInteractionHandlers::default());
    static AUTOMATION_HOOKS_INSTALLED: RefCell<bool> = const { RefCell::new(false) };
    static ACTIVE_TIMER_INTERVALS: RefCell<HashMap<String, i32>> = RefCell::new(HashMap::new());
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

    #[must_use]
    pub fn property_value(&self, id: NodeId, name: &str) -> Option<&str> {
        self.properties
            .get(&id)
            .and_then(|properties| properties.get(name))
            .and_then(|value| value.as_deref())
    }

    #[must_use]
    pub fn style_value(&self, id: NodeId, name: &str) -> Option<&str> {
        self.styles
            .get(&id)
            .and_then(|styles| styles.get(name))
            .and_then(|value| value.as_deref())
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

    pub fn event_ports_for(&self, id: NodeId) -> Vec<(EventPortId, UiEventKind)> {
        self.event_ports.get(&id).cloned().unwrap_or_default()
    }

    fn input_value_for(&self, id: NodeId) -> Option<&String> {
        self.input_values.get(&id)
    }

    fn checked_value_for(&self, id: NodeId) -> Option<bool> {
        self.checked_values.get(&id).copied()
    }

    fn selected_value_for(&self, id: NodeId) -> Option<&String> {
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
    let active_text_input = active_text_input_state();
    match root {
        RenderRoot::UiTree(node) => {
            render_ui_node(node, state, handlers, active_text_input.as_ref())
        }
        RenderRoot::SceneGraph(node) => render_scene_node(node),
    }
}

pub fn render_retained_snapshot_signal(
    snapshots: impl Signal<Item = (RenderRoot, FakeRenderState)> + 'static,
    handlers: RenderInteractionHandlers,
) -> impl Element {
    let host = Mutable::new(None::<web_sys::HtmlElement>);
    let mounted = Rc::new(RefCell::new(MountedRenderHost::default()));

    let task = Task::start_droppable({
        let host = host.clone();
        let mounted = mounted.clone();
        let handlers = handlers.clone();
        async move {
            let element = host.wait_for_some_cloned().await;
            snapshots
                .for_each_sync(move |(root, state)| {
                    install_automation_hooks(handlers.clone());
                    mounted
                        .borrow_mut()
                        .apply_snapshot(&element, &root, &state, &handlers);
                })
                .await;
        }
    });

    struct RetainedRenderHost {
        raw_el: zoon::RawHtmlEl<web_sys::HtmlElement>,
    }

    impl Element for RetainedRenderHost {}

    impl RawElWrapper for RetainedRenderHost {
        type RawEl = zoon::RawHtmlEl<web_sys::HtmlElement>;

        fn raw_el_mut(&mut self) -> &mut Self::RawEl {
            &mut self.raw_el
        }
    }

    RetainedRenderHost {
        raw_el: zoon::RawHtmlEl::new("div")
            .attr("data-boon-retained-host", "true")
            .after_insert({
                let host = host.clone();
                move |element| {
                    if let Some(element) = element.dyn_ref::<web_sys::HtmlElement>() {
                        host.set(Some(element.clone()));
                    }
                }
            })
            .after_remove({
                let host = host.clone();
                let mounted = mounted.clone();
                move |_| {
                    host.set(None);
                    mounted.borrow_mut().clear();
                    drop(task);
                }
            }),
    }
}

#[derive(Default)]
struct MountedRenderHost {
    root: Option<RenderRoot>,
    state: FakeRenderState,
    nodes: HashMap<NodeId, web_sys::Node>,
    inline_text: HashMap<NodeId, web_sys::Text>,
    attachments: HashMap<NodeId, Vec<NodeAttachment>>,
}

enum NodeAttachment {
    Listener {
        target: web_sys::EventTarget,
        event_name: String,
        callback: Closure<dyn FnMut(web_sys::Event)>,
    },
    Interval(i32),
}

impl MountedRenderHost {
    fn clear(&mut self) {
        let attachments = std::mem::take(&mut self.attachments);
        for (_, entries) in attachments {
            for entry in entries {
                detach_attachment(entry);
            }
        }
        self.root = None;
        self.state = FakeRenderState::default();
        self.nodes.clear();
        self.inline_text.clear();
    }

    fn apply_snapshot(
        &mut self,
        host: &web_sys::HtmlElement,
        new_root: &RenderRoot,
        new_state: &FakeRenderState,
        handlers: &RenderInteractionHandlers,
    ) {
        let old_root = self.root.clone();
        let old_state = self.state.clone();

        match old_root.as_ref() {
            Some(old_root) if roots_are_compatible(old_root, new_root) => {
                self.sync_root(host, old_root, new_root, &old_state, new_state, handlers);
            }
            _ => {
                self.replace_root(host, new_root, new_state, handlers);
            }
        }

        self.root = Some(new_root.clone());
        self.state = new_state.clone();
    }

    fn replace_root(
        &mut self,
        host: &web_sys::HtmlElement,
        root: &RenderRoot,
        state: &FakeRenderState,
        handlers: &RenderInteractionHandlers,
    ) {
        self.clear_host_children(host);
        let node = self.build_root_node(root, state, handlers);
        let _ = host.append_child(&node);
        if let RenderRoot::UiTree(root) = root {
            self.focus_inserted_ui_subtree(root, state);
        }
    }

    fn clear_host_children(&mut self, host: &web_sys::HtmlElement) {
        self.clear();
        while let Some(child) = host.first_child() {
            let _ = host.remove_child(&child);
        }
    }

    fn sync_root(
        &mut self,
        host: &web_sys::HtmlElement,
        old_root: &RenderRoot,
        new_root: &RenderRoot,
        old_state: &FakeRenderState,
        new_state: &FakeRenderState,
        handlers: &RenderInteractionHandlers,
    ) {
        match (old_root, new_root) {
            (RenderRoot::UiTree(old), RenderRoot::UiTree(new)) => {
                self.sync_ui_node(host.as_ref(), old, new, old_state, new_state, handlers);
            }
            (RenderRoot::SceneGraph(old), RenderRoot::SceneGraph(new)) => {
                self.sync_scene_node(host.as_ref(), old, new);
            }
            _ => self.replace_root(host, new_root, new_state, handlers),
        }
    }

    fn build_root_node(
        &mut self,
        root: &RenderRoot,
        state: &FakeRenderState,
        handlers: &RenderInteractionHandlers,
    ) -> web_sys::Node {
        match root {
            RenderRoot::UiTree(node) => self.build_ui_node(node, state, handlers),
            RenderRoot::SceneGraph(node) => self.build_scene_node(node),
        }
    }

    fn build_ui_node(
        &mut self,
        node: &UiNode,
        state: &FakeRenderState,
        handlers: &RenderInteractionHandlers,
    ) -> web_sys::Node {
        let document = web_sys::window()
            .and_then(|window| window.document())
            .expect("document should exist while rendering retained preview");
        match &node.kind {
            UiNodeKind::Text { text } => {
                let text_node: web_sys::Text = document.create_text_node(text);
                let dom_node: web_sys::Node = text_node.clone().into();
                self.nodes.insert(node.id, dom_node.clone());
                dom_node
            }
            UiNodeKind::Element { tag, text, .. } => {
                let element = document
                    .create_element(tag)
                    .expect("element tag should be valid");
                let dom_node: web_sys::Node = element.clone().into();
                self.nodes.insert(node.id, dom_node.clone());
                let node_id = node.id.0.to_string();
                let _ = element.set_attribute("data-boon-tag", tag);
                let _ = element.set_attribute("data-boon-node-id", &node_id);
                self.sync_inline_text(node.id, &element, None, text.as_deref());
                self.sync_ui_element_state(
                    &element,
                    node.id,
                    tag,
                    &FakeRenderState::default(),
                    state,
                    handlers,
                );
                for child in &node.children {
                    let child_node = self.build_ui_node(child, state, handlers);
                    let _ = element.append_child(&child_node);
                }
                dom_node
            }
        }
    }

    fn build_scene_node(&mut self, node: &SceneNode) -> web_sys::Node {
        let document = web_sys::window()
            .and_then(|window| window.document())
            .expect("document should exist while rendering retained preview");
        let element = document
            .create_element("div")
            .expect("scene wrapper should be creatable");
        let dom_node: web_sys::Node = element.clone().into();
        self.nodes.insert(node.id, dom_node.clone());
        let _ = element.set_attribute("data-boon-scene-node-id", &node.id.0.to_string());
        self.sync_inline_text(node.id, &element, None, Some(&scene_label(node.kind.clone())));
        for child in &node.children {
            let child_node = self.build_scene_node(child);
            let _ = element.append_child(&child_node);
        }
        dom_node
    }

    fn sync_ui_node(
        &mut self,
        parent: &web_sys::Node,
        old: &UiNode,
        new: &UiNode,
        old_state: &FakeRenderState,
        new_state: &FakeRenderState,
        handlers: &RenderInteractionHandlers,
    ) {
        if !ui_nodes_are_compatible(old, new) {
            self.replace_ui_subtree(parent, old, new, new_state, handlers);
            return;
        }

        match (&old.kind, &new.kind) {
            (UiNodeKind::Text { text: old_text }, UiNodeKind::Text { text: new_text }) => {
                if old_text != new_text
                    && let Some(node) = self.nodes.get(&new.id)
                    && let Some(text) = node.dyn_ref::<web_sys::Text>()
                {
                    text.set_data(new_text);
                }
            }
            (
                UiNodeKind::Element {
                    tag: old_tag,
                    text: old_text,
                    ..
                },
                UiNodeKind::Element {
                    tag: new_tag,
                    text: new_text,
                    ..
                },
            ) => {
                if old_tag != new_tag {
                    self.replace_ui_subtree(parent, old, new, new_state, handlers);
                    return;
                }
                let Some(node) = self.nodes.get(&new.id).cloned() else {
                    self.replace_ui_subtree(parent, old, new, new_state, handlers);
                    return;
                };
                let Some(element) = node.dyn_ref::<web_sys::Element>() else {
                    self.replace_ui_subtree(parent, old, new, new_state, handlers);
                    return;
                };

                self.sync_inline_text(new.id, element, old_text.as_deref(), new_text.as_deref());
                self.sync_ui_element_state(element, new.id, new_tag, old_state, new_state, handlers);
                self.sync_ui_children(
                    element.as_ref(),
                    old,
                    new,
                    old_state,
                    new_state,
                    handlers,
                );
            }
            _ => self.replace_ui_subtree(parent, old, new, new_state, handlers),
        }
    }

    fn sync_ui_children(
        &mut self,
        parent: &web_sys::Node,
        old: &UiNode,
        new: &UiNode,
        old_state: &FakeRenderState,
        new_state: &FakeRenderState,
        handlers: &RenderInteractionHandlers,
    ) {
        let new_ids = new.children.iter().map(|child| child.id).collect::<HashSet<_>>();
        for old_child in &old.children {
            if !new_ids.contains(&old_child.id) {
                self.remove_ui_subtree(parent, old_child);
            }
        }

        let old_children = old
            .children
            .iter()
            .map(|child| (child.id, child))
            .collect::<HashMap<_, _>>();

        for (index, new_child) in new.children.iter().enumerate() {
            if let Some(old_child) = old_children.get(&new_child.id) {
                self.sync_ui_node(parent, old_child, new_child, old_state, new_state, handlers);
            } else {
                let node = self.build_ui_node(new_child, new_state, handlers);
                let _ = insert_child_node(
                    parent,
                    child_dom_reference(parent, self.inline_child_offset(new.id), index).as_ref(),
                    &node,
                );
                self.focus_inserted_ui_subtree(new_child, new_state);
            }

            if let Some(node) = self.nodes.get(&new_child.id).cloned() {
                let reference = child_dom_reference(parent, self.inline_child_offset(new.id), index);
                if reference.as_ref() != Some(&node) {
                    let _ = insert_child_node(parent, reference.as_ref(), &node);
                }
            }
        }
    }

    fn replace_ui_subtree(
        &mut self,
        parent: &web_sys::Node,
        old: &UiNode,
        new: &UiNode,
        new_state: &FakeRenderState,
        handlers: &RenderInteractionHandlers,
    ) {
        self.blur_if_active_in_ui_subtree(old);
        let new_dom = self.build_ui_node(new, new_state, handlers);
        if let Some(old_dom) = self.nodes.get(&old.id).cloned() {
            let _ = parent.replace_child(&new_dom, &old_dom);
        } else {
            let _ = parent.append_child(&new_dom);
        }
        self.cleanup_ui_subtree(old);
        self.focus_inserted_ui_subtree(new, new_state);
    }

    fn remove_ui_subtree(&mut self, parent: &web_sys::Node, node: &UiNode) {
        self.blur_if_active_in_ui_subtree(node);
        if let Some(dom) = self.nodes.get(&node.id).cloned() {
            let _ = parent.remove_child(&dom);
        }
        self.cleanup_ui_subtree(node);
    }

    fn cleanup_ui_subtree(&mut self, node: &UiNode) {
        for child in &node.children {
            self.cleanup_ui_subtree(child);
        }
        self.clear_node_attachments(node.id);
        self.inline_text.remove(&node.id);
        self.nodes.remove(&node.id);
    }

    fn sync_scene_node(&mut self, parent: &web_sys::Node, old: &SceneNode, new: &SceneNode) {
        if !scene_nodes_are_compatible(old, new) {
            self.replace_scene_subtree(parent, old, new);
            return;
        }

        let Some(node) = self.nodes.get(&new.id).cloned() else {
            self.replace_scene_subtree(parent, old, new);
            return;
        };
        let Some(element) = node.dyn_ref::<web_sys::Element>() else {
            self.replace_scene_subtree(parent, old, new);
            return;
        };

        let old_label = scene_label(old.kind.clone());
        let new_label = scene_label(new.kind.clone());
        self.sync_inline_text(new.id, element, Some(old_label.as_str()), Some(new_label.as_str()));

        let new_ids = new.children.iter().map(|child| child.id).collect::<HashSet<_>>();
        for old_child in &old.children {
            if !new_ids.contains(&old_child.id) {
                self.remove_scene_subtree(element.as_ref(), old_child);
            }
        }
        let old_children = old
            .children
            .iter()
            .map(|child| (child.id, child))
            .collect::<HashMap<_, _>>();
        for (index, new_child) in new.children.iter().enumerate() {
            if let Some(old_child) = old_children.get(&new_child.id) {
                self.sync_scene_node(element.as_ref(), old_child, new_child);
            } else {
                let node = self.build_scene_node(new_child);
                let _ = insert_child_node(
                    element.as_ref(),
                    child_dom_reference(element.as_ref(), self.inline_child_offset(new.id), index)
                        .as_ref(),
                    &node,
                );
            }
            if let Some(node) = self.nodes.get(&new_child.id).cloned() {
                let reference =
                    child_dom_reference(element.as_ref(), self.inline_child_offset(new.id), index);
                if reference.as_ref() != Some(&node) {
                    let _ = insert_child_node(element.as_ref(), reference.as_ref(), &node);
                }
            }
        }
    }

    fn replace_scene_subtree(&mut self, parent: &web_sys::Node, old: &SceneNode, new: &SceneNode) {
        let new_dom = self.build_scene_node(new);
        if let Some(old_dom) = self.nodes.get(&old.id).cloned() {
            let _ = parent.replace_child(&new_dom, &old_dom);
        } else {
            let _ = parent.append_child(&new_dom);
        }
        self.cleanup_scene_subtree(old);
    }

    fn remove_scene_subtree(&mut self, parent: &web_sys::Node, node: &SceneNode) {
        if let Some(dom) = self.nodes.get(&node.id).cloned() {
            let _ = parent.remove_child(&dom);
        }
        self.cleanup_scene_subtree(node);
    }

    fn cleanup_scene_subtree(&mut self, node: &SceneNode) {
        for child in &node.children {
            self.cleanup_scene_subtree(child);
        }
        self.inline_text.remove(&node.id);
        self.nodes.remove(&node.id);
    }

    fn sync_inline_text(
        &mut self,
        node_id: NodeId,
        element: &web_sys::Element,
        old_text: Option<&str>,
        new_text: Option<&str>,
    ) {
        if old_text == new_text && self.inline_text.contains_key(&node_id) == new_text.is_some() {
            return;
        }

        if let Some(text_node) = self.inline_text.remove(&node_id) {
            let _ = element.remove_child(&text_node);
        }

        if let Some(text) = new_text {
            let document = web_sys::window()
                .and_then(|window| window.document())
                .expect("document should exist while rendering retained preview");
            let text_node = document.create_text_node(text);
            let text_node_node: web_sys::Node = text_node.clone().into();
            let reference = element.first_child();
            let _ = insert_child_node(element.as_ref(), reference.as_ref(), &text_node_node);
            self.inline_text.insert(node_id, text_node);
        }
    }

    fn inline_child_offset(&self, node_id: NodeId) -> usize {
        usize::from(self.inline_text.contains_key(&node_id))
    }

    fn sync_ui_element_state(
        &mut self,
        element: &web_sys::Element,
        node_id: NodeId,
        tag: &str,
        old_state: &FakeRenderState,
        new_state: &FakeRenderState,
        handlers: &RenderInteractionHandlers,
    ) {
        let _ = element.set_attribute("data-boon-tag", tag);
        let _ = element.set_attribute("data-boon-node-id", &node_id.0.to_string());

        sync_attributes(element, node_id, old_state, new_state);
        sync_styles(element, node_id, old_state, new_state);
        sync_classes(element, node_id, old_state, new_state);
        sync_port_attributes(element, node_id, new_state);
        sync_input_like_state(element, node_id, tag, old_state, new_state);
        self.refresh_node_attachments(element, node_id, new_state, handlers);
    }

    fn refresh_node_attachments(
        &mut self,
        element: &web_sys::Element,
        node_id: NodeId,
        state: &FakeRenderState,
        handlers: &RenderInteractionHandlers,
    ) {
        self.clear_node_attachments(node_id);
        let mut entries = Vec::new();
        attach_fact_listeners(&mut entries, element, node_id, handlers.clone());
        for (port, kind) in state.event_ports_for(node_id) {
            attach_port_listener(&mut entries, element, node_id, port, kind, handlers.clone());
        }
        if !entries.is_empty() {
            self.attachments.insert(node_id, entries);
        }
    }

    fn clear_node_attachments(&mut self, node_id: NodeId) {
        if let Some(entries) = self.attachments.remove(&node_id) {
            for entry in entries {
                detach_attachment(entry);
            }
        }
    }

    fn focus_inserted_ui_subtree(&self, node: &UiNode, state: &FakeRenderState) -> bool {
        match &node.kind {
            UiNodeKind::Text { .. } => false,
            UiNodeKind::Element { .. } => {
                for child in &node.children {
                    if self.focus_inserted_ui_subtree(child, state) {
                        return true;
                    }
                }
                let wants_autofocus = state.property_value(node.id, "autofocus") == Some("true");
                let wants_focused = state.property_value(node.id, "focused") == Some("true");
                if !wants_autofocus && !wants_focused {
                    return false;
                }
                let Some(dom) = self.nodes.get(&node.id) else {
                    return false;
                };
                if !dom.is_connected() {
                    return false;
                }
                if let Some(input) = dom.dyn_ref::<web_sys::HtmlInputElement>() {
                    let _ = input.focus();
                    return true;
                }
                if let Some(html) = dom.dyn_ref::<web_sys::HtmlElement>() {
                    let _ = html.focus();
                    return true;
                }
                false
            }
        }
    }

    fn blur_if_active_in_ui_subtree(&self, node: &UiNode) {
        let Some(active_node_id) = active_text_input_node_id() else {
            return;
        };
        if !ui_subtree_contains_node_id(node, &active_node_id) {
            return;
        }
        let Some(window) = web_sys::window() else {
            return;
        };
        let Some(document) = window.document() else {
            return;
        };
        let Some(active) = document.active_element() else {
            return;
        };
        if let Some(input) = active.dyn_ref::<web_sys::HtmlInputElement>() {
            let _ = input.blur();
        } else if let Some(textarea) = active.dyn_ref::<web_sys::HtmlTextAreaElement>() {
            let _ = textarea.blur();
        } else if let Some(html) = active.dyn_ref::<web_sys::HtmlElement>() {
            let _ = html.blur();
        }
    }
}

fn roots_are_compatible(old: &RenderRoot, new: &RenderRoot) -> bool {
    match (old, new) {
        (RenderRoot::UiTree(old), RenderRoot::UiTree(new)) => ui_nodes_are_compatible(old, new),
        (RenderRoot::SceneGraph(old), RenderRoot::SceneGraph(new)) => {
            scene_nodes_are_compatible(old, new)
        }
        _ => false,
    }
}

fn ui_nodes_are_compatible(old: &UiNode, new: &UiNode) -> bool {
    match (&old.kind, &new.kind) {
        (UiNodeKind::Text { .. }, UiNodeKind::Text { .. }) => old.id == new.id,
        (UiNodeKind::Element { tag: old_tag, .. }, UiNodeKind::Element { tag: new_tag, .. }) => {
            old.id == new.id && old_tag == new_tag
        }
        _ => false,
    }
}

fn ui_subtree_contains_node_id(node: &UiNode, node_id: &str) -> bool {
    if node.id.0.to_string() == node_id {
        return true;
    }
    node.children
        .iter()
        .any(|child| ui_subtree_contains_node_id(child, node_id))
}

fn scene_nodes_are_compatible(old: &SceneNode, new: &SceneNode) -> bool {
    old.id == new.id
}

fn scene_label(kind: SceneNodeKind) -> String {
    match kind {
        SceneNodeKind::Group => "[scene-group]".to_string(),
        SceneNodeKind::Primitive { primitive } => format!("[primitive: {primitive}]"),
        SceneNodeKind::Label { text } => text,
    }
}

fn child_dom_reference(
    parent: &web_sys::Node,
    inline_offset: usize,
    child_index: usize,
) -> Option<web_sys::Node> {
    parent.child_nodes().item((inline_offset + child_index) as u32)
}

fn insert_child_node(
    parent: &web_sys::Node,
    reference: Option<&web_sys::Node>,
    child: &web_sys::Node,
) -> Result<(), wasm_bindgen::JsValue> {
    match reference {
        Some(reference) => parent.insert_before(child, Some(reference)).map(|_| ()),
        None => parent.append_child(child).map(|_| ()),
    }
}

fn sync_attributes(
    element: &web_sys::Element,
    node_id: NodeId,
    old_state: &FakeRenderState,
    new_state: &FakeRenderState,
) {
    let mut names = old_state
        .properties_for(node_id)
        .map(|(name, _)| name.clone())
        .collect::<HashSet<_>>();
    names.extend(
        new_state
            .properties_for(node_id)
            .map(|(name, _)| name.clone())
            .collect::<HashSet<_>>(),
    );
    for name in names {
        match new_state.property_value(node_id, &name) {
            Some(value) => {
                let _ = element.set_attribute(&name, value);
            }
            None => {
                let _ = element.remove_attribute(&name);
            }
        }
    }
}

fn sync_styles(
    element: &web_sys::Element,
    node_id: NodeId,
    old_state: &FakeRenderState,
    new_state: &FakeRenderState,
) {
    let Some(html) = element.dyn_ref::<web_sys::HtmlElement>() else {
        return;
    };
    let style = html.style();
    let mut names = old_state
        .styles_for(node_id)
        .map(|(name, _)| name.clone())
        .collect::<HashSet<_>>();
    names.extend(
        new_state
            .styles_for(node_id)
            .map(|(name, _)| name.clone())
            .collect::<HashSet<_>>(),
    );
    for name in names {
        match new_state.style_value(node_id, &name) {
            Some(value) => {
                let _ = style.set_property(&name, value);
            }
            None => {
                let _ = style.remove_property(&name);
            }
        }
    }
}

fn sync_classes(
    element: &web_sys::Element,
    node_id: NodeId,
    old_state: &FakeRenderState,
    new_state: &FakeRenderState,
) {
    let old_classes = old_state.enabled_classes_for(node_id).cloned().collect::<Vec<_>>();
    let new_classes = new_state.enabled_classes_for(node_id).cloned().collect::<Vec<_>>();
    if old_classes == new_classes {
        return;
    }
    if new_classes.is_empty() {
        let _ = element.remove_attribute("class");
    } else {
        let _ = element.set_attribute("class", &new_classes.join(" "));
    }
}

fn sync_port_attributes(element: &web_sys::Element, node_id: NodeId, state: &FakeRenderState) {
    for kind in [
        UiEventKind::Click,
        UiEventKind::DoubleClick,
        UiEventKind::Input,
        UiEventKind::Change,
        UiEventKind::KeyDown,
        UiEventKind::Blur,
        UiEventKind::Focus,
        UiEventKind::Custom("custom".to_string()),
    ] {
        let _ = element.remove_attribute(event_port_attr_name(&kind));
    }
    for (port, kind) in state.event_ports_for(node_id) {
        let _ = element.set_attribute(event_port_attr_name(&kind), &port.0.to_string());
    }
}

fn sync_input_like_state(
    element: &web_sys::Element,
    node_id: NodeId,
    tag: &str,
    old_state: &FakeRenderState,
    new_state: &FakeRenderState,
) {
    if tag == "input" {
        if let Some(input) = element.dyn_ref::<web_sys::HtmlInputElement>() {
            if let Some(value) = new_state.input_value_for(node_id) {
                sync_active_input_value(input, &node_id.0.to_string(), value);
            }
            if let Some(checked) = new_state.checked_value_for(node_id) {
                input.set_checked(checked);
                let _ = element.set_attribute("aria-checked", if checked { "true" } else { "false" });
                let _ = element.set_attribute("data-checked", if checked { "true" } else { "false" });
            } else {
                let _ = element.remove_attribute("aria-checked");
                let _ = element.remove_attribute("data-checked");
            }

            let was_autofocus = old_state.property_value(node_id, "autofocus") == Some("true");
            let wants_autofocus = new_state.property_value(node_id, "autofocus") == Some("true");
            let was_focused = old_state.property_value(node_id, "focused") == Some("true");
            let wants_focused = new_state.property_value(node_id, "focused") == Some("true");
            let connected = element.is_connected();
            if connected && wants_autofocus && !was_autofocus {
                let _ = input.focus();
            } else if connected && wants_focused && !was_focused {
                let _ = input.focus();
            }
        }
    }

    if tag == "select"
        && let Some(select) = element.dyn_ref::<web_sys::HtmlSelectElement>()
        && let Some(value) = new_state.selected_value_for(node_id)
        && select.value() != *value
    {
        select.set_value(value);
    }
}

fn sync_active_input_value(input: &web_sys::HtmlInputElement, node_id: &str, value: &str) {
    if input.value() == value {
        return;
    }
    if active_text_input_node_id().as_deref() == Some(node_id) {
        // While the user is actively typing, keep the live browser-owned draft instead of
        // continuously replaying the model value back into the focused input. That avoids
        // fighting fast key-repeat and caret motion on every rerender. Explicit clears still
        // flow through so submit-style inputs can reset in place.
        if value.is_empty() {
            input.set_value(value);
        }
        return;
    }
    input.set_value(value);
}

fn attach_fact_listeners(
    entries: &mut Vec<NodeAttachment>,
    element: &web_sys::Element,
    node_id: NodeId,
    handlers: RenderInteractionHandlers,
) {
    let target: web_sys::EventTarget = element.clone().into();
    let mouse_enter = Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new({
        let handlers = handlers.clone();
        move |_| {
            handlers.emit_fact(UiFact {
                id: node_id,
                kind: UiFactKind::Hovered(true),
            });
        }
    }));
    let _ = target.add_event_listener_with_callback("mouseenter", mouse_enter.as_ref().unchecked_ref());
    entries.push(NodeAttachment::Listener {
        target: target.clone(),
        event_name: "mouseenter".to_string(),
        callback: mouse_enter,
    });

    let mouse_leave = Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new({
        let handlers = handlers.clone();
        move |_| {
            handlers.emit_fact(UiFact {
                id: node_id,
                kind: UiFactKind::Hovered(false),
            });
        }
    }));
    let _ = target.add_event_listener_with_callback("mouseleave", mouse_leave.as_ref().unchecked_ref());
    entries.push(NodeAttachment::Listener {
        target: target.clone(),
        event_name: "mouseleave".to_string(),
        callback: mouse_leave,
    });

    let focus = Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new({
        let handlers = handlers.clone();
        move |_| {
            handlers.emit_fact(UiFact {
                id: node_id,
                kind: UiFactKind::Focused(true),
            });
        }
    }));
    let _ = target.add_event_listener_with_callback("focus", focus.as_ref().unchecked_ref());
    entries.push(NodeAttachment::Listener {
        target: target.clone(),
        event_name: "focus".to_string(),
        callback: focus,
    });

    let blur = Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(move |event| {
        let handlers = handlers.clone();
        defer_real_blur(event.target(), node_id, move || {
            handlers.emit_fact(UiFact {
                id: node_id,
                kind: UiFactKind::Focused(false),
            });
        });
    }));
    let _ = target.add_event_listener_with_callback("blur", blur.as_ref().unchecked_ref());
    entries.push(NodeAttachment::Listener {
        target,
        event_name: "blur".to_string(),
        callback: blur,
    });
}

fn attach_port_listener(
    entries: &mut Vec<NodeAttachment>,
    element: &web_sys::Element,
    node_id: NodeId,
    port: EventPortId,
    kind: UiEventKind,
    handlers: RenderInteractionHandlers,
) {
    let target: web_sys::EventTarget = element.clone().into();
    match kind {
        UiEventKind::Click => {
            let callback = Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(move |event| {
                let payload = event
                    .dyn_ref::<web_sys::MouseEvent>()
                    .map(|event| format!("{{\"x\":{},\"y\":{}}}", event.offset_x(), event.offset_y()));
                handlers.emit_event(UiEvent {
                    target: port,
                    kind: UiEventKind::Click,
                    payload,
                });
            }));
            let _ = target.add_event_listener_with_callback("click", callback.as_ref().unchecked_ref());
            entries.push(NodeAttachment::Listener {
                target,
                event_name: "click".to_string(),
                callback,
            });
        }
        UiEventKind::DoubleClick => {
            let callback = Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(move |_| {
                handlers.emit_event(UiEvent {
                    target: port,
                    kind: UiEventKind::DoubleClick,
                    payload: None,
                });
            }));
            let _ = target.add_event_listener_with_callback("dblclick", callback.as_ref().unchecked_ref());
            entries.push(NodeAttachment::Listener {
                target,
                event_name: "dblclick".to_string(),
                callback,
            });
        }
        UiEventKind::Input => {
            let callback = Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(move |event| {
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
            }));
            let _ = target.add_event_listener_with_callback("input", callback.as_ref().unchecked_ref());
            entries.push(NodeAttachment::Listener {
                target,
                event_name: "input".to_string(),
                callback,
            });
        }
        UiEventKind::Change => {
            let callback = Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(move |event| {
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
            }));
            let _ = target.add_event_listener_with_callback("change", callback.as_ref().unchecked_ref());
            entries.push(NodeAttachment::Listener {
                target,
                event_name: "change".to_string(),
                callback,
            });
        }
        UiEventKind::Blur => {
            let callback = Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(move |event| {
                let handlers = handlers.clone();
                defer_real_blur(event.target(), node_id, move || {
                    handlers.emit_event(UiEvent {
                        target: port,
                        kind: UiEventKind::Blur,
                        payload: None,
                    });
                });
            }));
            let _ = target.add_event_listener_with_callback("blur", callback.as_ref().unchecked_ref());
            entries.push(NodeAttachment::Listener {
                target,
                event_name: "blur".to_string(),
                callback,
            });
        }
        UiEventKind::Focus => {
            let callback = Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(move |_| {
                handlers.emit_event(UiEvent {
                    target: port,
                    kind: UiEventKind::Focus,
                    payload: None,
                });
            }));
            let _ = target.add_event_listener_with_callback("focus", callback.as_ref().unchecked_ref());
            entries.push(NodeAttachment::Listener {
                target,
                event_name: "focus".to_string(),
                callback,
            });
        }
        UiEventKind::KeyDown => {
            let listener_target = target.clone();
            let callback = Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(move |event| {
                let payload = event
                    .dyn_ref::<web_sys::KeyboardEvent>()
                    .map(|event| {
                        encode_key_down_payload(
                            &event.key(),
                            input_event_value(Some(listener_target.clone())).as_deref(),
                        )
                    });
                handlers.emit_event(UiEvent {
                    target: port,
                    kind: UiEventKind::KeyDown,
                    payload,
                });
            }));
            let _ = target.add_event_listener_with_callback("keydown", callback.as_ref().unchecked_ref());
            entries.push(NodeAttachment::Listener {
                target,
                event_name: "keydown".to_string(),
                callback,
            });
        }
        UiEventKind::Custom(name) => {
            if let Some(interval_ms) = name
                .strip_prefix("timer:")
                .and_then(|value| value.parse::<i32>().ok())
                && let Some(window) = web_sys::window()
            {
                let callback = Closure::<dyn FnMut()>::wrap(Box::new(move || {
                    handlers.emit_event(UiEvent {
                        target: port,
                        kind: UiEventKind::Custom(name.clone()),
                        payload: None,
                    });
                }));
                if let Ok(interval_id) = window
                    .set_interval_with_callback_and_timeout_and_arguments_0(
                        callback.as_ref().unchecked_ref(),
                        interval_ms,
                    )
                {
                    callback.forget();
                    entries.push(NodeAttachment::Interval(interval_id));
                }
            }
        }
    }
}

fn detach_attachment(attachment: NodeAttachment) {
    match attachment {
        NodeAttachment::Listener {
            target,
            event_name,
            callback,
        } => {
            let _ = target.remove_event_listener_with_callback(
                &event_name,
                callback.as_ref().unchecked_ref(),
            );
        }
        NodeAttachment::Interval(id) => {
            if let Some(window) = web_sys::window() {
                window.clear_interval_with_handle(id);
            }
        }
    }
}

#[derive(Debug, Clone)]
struct ActiveTextInputState {
    node_id: String,
    selection_start: Option<u32>,
    selection_end: Option<u32>,
}

fn active_text_input_state() -> Option<ActiveTextInputState> {
    let window = web_sys::window()?;
    let document = window.document()?;
    let active = document.active_element()?;

    let node_id = active.get_attribute("data-boon-node-id")?;
    let (selection_start, selection_end) =
        if let Some(input) = active.dyn_ref::<web_sys::HtmlInputElement>() {
            (
                input.selection_start().ok().flatten(),
                input.selection_end().ok().flatten(),
            )
        } else if let Some(textarea) = active.dyn_ref::<web_sys::HtmlTextAreaElement>() {
            (
                textarea.selection_start().ok().flatten(),
                textarea.selection_end().ok().flatten(),
            )
        } else {
            return None;
        };

    Some(ActiveTextInputState {
        node_id,
        selection_start,
        selection_end,
    })
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
    active_text_input: Option<&ActiveTextInputState>,
) -> zoon::RawElOrText {
    match &node.kind {
        UiNodeKind::Text { text } => zoon::Text::new(text.clone()).unify(),
        UiNodeKind::Element { tag, text, .. } => {
            let children: Vec<zoon::RawElOrText> = node
                .children
                .iter()
                .map(|child| render_ui_node(child, state, handlers, active_text_input))
                .collect();
            let node_id_value = node.id;
            let node_id = node.id.0.to_string();
            let mut should_focus_after_insert = false;
            let mut should_select_after_insert = false;
            let restore_selection_after_insert = if tag == "input" {
                active_text_input
                    .filter(|input| input.node_id == node_id)
                    .map(|input| (input.selection_start, input.selection_end))
            } else {
                None
            };
            let mut input_value_after_insert = None;
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
                    move |event: zoon::events::Blur| {
                        let handlers = handlers.clone();
                        defer_real_blur(event.target(), node_id_value, move || {
                            handlers.emit_fact(UiFact {
                                id: node_id_value,
                                kind: UiFactKind::Focused(false),
                            });
                        });
                    }
                });
            for (name, value) in state.properties_for(node_id_value) {
                if let Some(value) = value {
                    if tag == "input" && name == "autofocus" && value == "true" {
                        should_focus_after_insert = true;
                        should_select_after_insert = true;
                    }
                    if tag == "input" && name == "focused" && value == "true" {
                        should_focus_after_insert = true;
                    }
                    if tag == "input" && name == "value" {
                        input_value_after_insert = Some(value.clone());
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
                    input_value_after_insert = Some(value.clone());
                    el = el.attr("value", value);
                }
                if should_focus_after_insert {
                    el = el.attr("data-boon-focused", "true").attr("focused", "true");
                }
            }
            if let Some(checked) = state.checked_value_for(node_id_value) {
                let checked = if checked { "true" } else { "false" };
                el = el
                    .attr("aria-checked", checked)
                    .attr("data-checked", checked);
            }
            if tag == "select" {
                let selected_value = state.selected_value_for(node_id_value).cloned();
                if let Some(selected_value) = selected_value {
                    el = el.after_insert(move |element| {
                        if let Some(select) = element.dyn_ref::<web_sys::HtmlSelectElement>() {
                            select.set_value(&selected_value);
                        }
                    });
                }
            }
            for (port, kind) in state.event_ports_for(node_id_value) {
                el = el.attr(event_port_attr_name(&kind), &port.0.to_string());
                el = attach_ui_handler(el, handlers.clone(), node_id_value, port, kind);
            }
            let should_restore_focus =
                should_focus_after_insert || restore_selection_after_insert.is_some();
            // Delayed focus replays help freshly-mounted autofocus inputs settle after layout,
            // but they can fight active typing by restoring an older caret position after a
            // newer keystroke has already advanced the selection.
            let schedule_delayed_focus_restore =
                should_focus_after_insert && restore_selection_after_insert.is_none();
            if input_value_after_insert.is_some() || should_restore_focus {
                let restore_selection_after_insert = restore_selection_after_insert;
                let input_value_after_insert = input_value_after_insert;
                el = el.after_insert(move |element| {
                    if let Some(input) = element.dyn_ref::<web_sys::HtmlInputElement>() {
                        if let Some(value) = input_value_after_insert.as_deref() {
                            input.set_value(value);
                        }
                        if should_restore_focus {
                            let _ = input.focus();
                            restore_input_selection(
                                input,
                                restore_selection_after_insert,
                                should_select_after_insert,
                            );
                        }
                    } else if should_restore_focus {
                        if let Some(html) = element.dyn_ref::<web_sys::HtmlElement>() {
                            let _ = html.focus();
                        }
                    }
                    if schedule_delayed_focus_restore {
                        if let Some(window) = web_sys::window() {
                            let delayed_element = element.clone();
                            let restore_selection_after_insert = restore_selection_after_insert;
                            let input_value_after_insert = input_value_after_insert.clone();
                            for delay_ms in [0, 50] {
                                let delayed_element = delayed_element.clone();
                                let restore_selection_after_insert = restore_selection_after_insert;
                                let input_value_after_insert = input_value_after_insert.clone();
                                let callback = wasm_bindgen::closure::Closure::once(move || {
                                    if let Some(input) =
                                        delayed_element.dyn_ref::<web_sys::HtmlInputElement>()
                                    {
                                        if let Some(value) = input_value_after_insert.as_deref() {
                                            input.set_value(value);
                                        }
                                        let _ = input.focus();
                                        restore_input_selection(
                                            input,
                                            restore_selection_after_insert,
                                            should_select_after_insert,
                                        );
                                    } else if let Some(html) =
                                        delayed_element.dyn_ref::<web_sys::HtmlElement>()
                                    {
                                        let _ = html.focus();
                                    }
                                });
                                let _ = window
                                    .set_timeout_with_callback_and_timeout_and_arguments_0(
                                        callback.as_ref().unchecked_ref(),
                                        delay_ms,
                                    );
                                callback.forget();
                            }
                        }
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

fn restore_input_selection(
    input: &web_sys::HtmlInputElement,
    restore_selection_after_insert: Option<(Option<u32>, Option<u32>)>,
    should_select_after_insert: bool,
) {
    if let Some((start, end)) = restore_selection_after_insert
        && let (Some(start), Some(end)) = (start, end)
    {
        let _ = input.set_selection_range(start, end);
        return;
    }

    if should_select_after_insert {
        let _ = input.select();
        return;
    }

    let caret = input.value().len() as u32;
    let _ = input.set_selection_range(caret, caret);
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
        "option" => zoon::RawHtmlEl::new("option"),
        "section" => zoon::RawHtmlEl::new("section"),
        "select" => zoon::RawHtmlEl::new("select"),
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
        UiEventKind::Click => raw_el.event_handler(move |event: zoon::events::Click| {
            handlers.emit_event(UiEvent {
                target: port,
                kind: UiEventKind::Click,
                payload: Some(format!(
                    "{{\"x\":{},\"y\":{}}}",
                    event.offset_x(),
                    event.offset_y()
                )),
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
        UiEventKind::Blur => raw_el.event_handler(move |event: zoon::events::Blur| {
            let handlers = handlers.clone();
            defer_real_blur(event.target(), node_id, move || {
                handlers.emit_event(UiEvent {
                    target: port,
                    kind: UiEventKind::Blur,
                    payload: None,
                });
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
        UiEventKind::Custom(name) => {
            if let Some(interval_ms) = name
                .strip_prefix("timer:")
                .and_then(|value| value.parse::<i32>().ok())
            {
                let port_key = port.0.to_string();
                let port_key_for_remove = port_key.clone();
                raw_el
                    .after_insert(move |element| {
                        let Some(window) = web_sys::window() else {
                            return;
                        };
                        let handlers = handlers.clone();
                        let element = element.clone();
                        let kind_name = name.clone();
                        let port_key_for_callback = port_key.clone();
                        ACTIVE_TIMER_INTERVALS.with(|timers| {
                            if let Some(existing_id) = timers.borrow_mut().remove(&port_key) {
                                window.clear_interval_with_handle(existing_id);
                            }
                        });
                        let interval_id = Rc::new(RefCell::new(None::<i32>));
                        let interval_id_for_callback = interval_id.clone();
                        let callback = Closure::<dyn FnMut()>::wrap(Box::new(move || {
                            if !element.is_connected() {
                                if let Some(id) = *interval_id_for_callback.borrow() {
                                    if let Some(window) = web_sys::window() {
                                        window.clear_interval_with_handle(id);
                                    }
                                    ACTIVE_TIMER_INTERVALS.with(|timers| {
                                        let mut timers = timers.borrow_mut();
                                        if timers.get(&port_key_for_callback).copied() == Some(id) {
                                            timers.remove(&port_key_for_callback);
                                        }
                                    });
                                }
                                return;
                            }
                            handlers.emit_event(UiEvent {
                                target: port,
                                kind: UiEventKind::Custom(kind_name.clone()),
                                payload: None,
                            });
                        }));
                        if let Ok(id) = window
                            .set_interval_with_callback_and_timeout_and_arguments_0(
                                callback.as_ref().unchecked_ref(),
                                interval_ms,
                            )
                        {
                            ACTIVE_TIMER_INTERVALS.with(|timers| {
                                timers.borrow_mut().insert(port_key.clone(), id);
                            });
                            *interval_id.borrow_mut() = Some(id);
                            callback.forget();
                        }
                    })
                    .after_remove(move |_| {
                        if let Some(window) = web_sys::window() {
                            ACTIVE_TIMER_INTERVALS.with(|timers| {
                                if let Some(existing_id) =
                                    timers.borrow_mut().remove(&port_key_for_remove)
                                {
                                    window.clear_interval_with_handle(existing_id);
                                }
                            });
                        }
                    })
            } else {
                raw_el
            }
        }
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

fn event_target_is_connected(target: Option<web_sys::EventTarget>) -> bool {
    target
        .and_then(|target| target.dyn_into::<web_sys::Node>().ok())
        .is_some_and(|node| node.is_connected())
}

fn active_text_input_node_id() -> Option<String> {
    let document = web_sys::window()?.document()?;
    let active = document.active_element()?;
    let tag = active.tag_name();
    if tag == "INPUT" || tag == "TEXTAREA" {
        active.get_attribute("data-boon-node-id")
    } else {
        None
    }
}

fn defer_real_blur(
    target: Option<web_sys::EventTarget>,
    node_id: NodeId,
    callback: impl FnOnce() + 'static,
) {
    if !event_target_is_connected(target.clone()) {
        return;
    }
    let expected_node_id = node_id.0.to_string();
    let callback = Closure::once(move || {
        if !event_target_is_connected(target) {
            return;
        }
        if active_text_input_node_id().as_deref() == Some(expected_node_id.as_str()) {
            return;
        }
        callback();
    });
    if let Some(window) = web_sys::window() {
        let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
            callback.as_ref().unchecked_ref(),
            16,
        );
    }
    callback.forget();
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
