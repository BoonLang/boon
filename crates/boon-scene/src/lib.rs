use serde::{Deserialize, Serialize};
use ulid::Ulid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RenderSurface {
    Document,
    Scene,
}

impl RenderSurface {
    #[must_use]
    pub const fn is_scene(self) -> bool {
        matches!(self, Self::Scene)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderRootHandle<T> {
    pub surface: RenderSurface,
    pub root: T,
    pub scene: Option<SceneHandles<T>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SceneHandles<T> {
    pub lights: Option<T>,
    pub geometry: Option<T>,
}

impl<T> RenderRootHandle<T> {
    #[must_use]
    pub const fn new(surface: RenderSurface, root: T) -> Self {
        Self {
            surface,
            root,
            scene: None,
        }
    }

    #[must_use]
    pub const fn scene(root: T, lights: Option<T>, geometry: Option<T>) -> Self {
        Self {
            surface: RenderSurface::Scene,
            root,
            scene: Some(SceneHandles { lights, geometry }),
        }
    }

    #[must_use]
    pub const fn is_scene(&self) -> bool {
        self.surface.is_scene()
    }

    #[must_use]
    pub fn map<U>(self, mut f: impl FnMut(T) -> U) -> RenderRootHandle<U> {
        RenderRootHandle {
            surface: self.surface,
            root: f(self.root),
            scene: self.scene.map(|scene| SceneHandles {
                lights: scene.lights.map(&mut f),
                geometry: scene.geometry.map(&mut f),
            }),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PhysicalSceneParams {
    pub shadow_dx_per_depth: f64,
    pub shadow_dy_per_depth: f64,
    pub shadow_blur_per_depth: f64,
    pub directional_intensity: f64,
    pub ambient_factor: f64,
    pub bevel_angle: f64,
}

impl PhysicalSceneParams {
    pub const DEFAULT: Self = Self {
        shadow_dx_per_depth: 1.5,
        shadow_dy_per_depth: 2.0,
        shadow_blur_per_depth: 3.0,
        directional_intensity: 0.8,
        ambient_factor: 0.3,
        bevel_angle: 135.0,
    };

    #[must_use]
    pub const fn shadow_opacity(self) -> f64 {
        let opacity = self.directional_intensity * (1.0 - self.ambient_factor) * 0.3;
        if opacity < 0.5 { opacity } else { 0.5 }
    }
}

impl Default for PhysicalSceneParams {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub Ulid);

impl NodeId {
    #[must_use]
    pub fn new() -> Self {
        Self(Ulid::new())
    }
}

impl Default for NodeId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EventPortId(pub Ulid);

impl EventPortId {
    #[must_use]
    pub fn new() -> Self {
        Self(Ulid::new())
    }
}

impl Default for EventPortId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RenderRoot {
    UiTree(UiNode),
    SceneGraph(SceneNode),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiNode {
    pub id: NodeId,
    pub kind: UiNodeKind,
    pub children: Vec<UiNode>,
}

impl UiNode {
    #[must_use]
    pub fn new(kind: UiNodeKind) -> Self {
        Self {
            id: NodeId::new(),
            kind,
            children: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_children(mut self, children: Vec<UiNode>) -> Self {
        self.children = children;
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UiNodeKind {
    Element {
        tag: String,
        text: Option<String>,
        event_ports: Vec<EventPortId>,
    },
    Text {
        text: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneNode {
    pub id: NodeId,
    pub kind: SceneNodeKind,
    pub children: Vec<SceneNode>,
}

impl SceneNode {
    #[must_use]
    pub fn new(kind: SceneNodeKind) -> Self {
        Self {
            id: NodeId::new(),
            kind,
            children: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_children(mut self, children: Vec<SceneNode>) -> Self {
        self.children = children;
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SceneNodeKind {
    Group,
    Primitive { primitive: String },
    Label { text: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SceneDiff {
    ReplaceRoot(RenderRoot),
    InsertNode {
        parent: NodeId,
        index: usize,
        node: RenderNode,
    },
    RemoveNode {
        id: NodeId,
    },
    UpdateText {
        id: NodeId,
        text: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderDiffBatch {
    pub ops: Vec<RenderOp>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RenderOp {
    ReplaceRoot(RenderRoot),
    InsertChild {
        parent: NodeId,
        index: usize,
        node: RenderNode,
    },
    RemoveNode {
        id: NodeId,
    },
    MoveChild {
        parent: NodeId,
        id: NodeId,
        index: usize,
    },
    SetText {
        id: NodeId,
        text: String,
    },
    SetProperty {
        id: NodeId,
        name: String,
        value: Option<String>,
    },
    SetStyle {
        id: NodeId,
        name: String,
        value: Option<String>,
    },
    SetClassFlag {
        id: NodeId,
        class_name: String,
        enabled: bool,
    },
    AttachEventPort {
        id: NodeId,
        port: EventPortId,
        kind: UiEventKind,
    },
    DetachEventPort {
        id: NodeId,
        port: EventPortId,
    },
    SetInputValue {
        id: NodeId,
        value: String,
    },
    SetChecked {
        id: NodeId,
        checked: bool,
    },
    SetSelectedIndex {
        id: NodeId,
        index: Option<usize>,
    },
    UpdateSceneParam {
        name: String,
        value: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RenderNode {
    Ui(UiNode),
    Scene(SceneNode),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiEventBatch {
    pub events: Vec<UiEvent>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiEvent {
    pub target: EventPortId,
    pub kind: UiEventKind,
    pub payload: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UiEventKind {
    Click,
    DoubleClick,
    Input,
    Change,
    KeyDown,
    Blur,
    Focus,
    Custom(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiFactBatch {
    pub facts: Vec<UiFact>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiFact {
    pub id: NodeId,
    pub kind: UiFactKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UiFactKind {
    Hovered(bool),
    Focused(bool),
    DraftText(String),
    LayoutSize { width: i32, height: i32 },
    Custom { name: String, value: String },
}

#[cfg(test)]
mod tests {
    use super::{
        EventPortId, NodeId, PhysicalSceneParams, RenderDiffBatch, RenderNode, RenderOp,
        RenderRoot, RenderRootHandle, RenderSurface, SceneNode, SceneNodeKind, UiEventKind, UiNode,
        UiNodeKind,
    };

    fn sample_ui_root() -> RenderRoot {
        RenderRoot::UiTree(
            UiNode::new(UiNodeKind::Element {
                tag: "button".to_string(),
                text: Some("Press".to_string()),
                event_ports: Vec::new(),
            })
            .with_children(vec![UiNode::new(UiNodeKind::Text {
                text: "Child".to_string(),
            })]),
        )
    }

    fn sample_scene_root() -> RenderRoot {
        RenderRoot::SceneGraph(SceneNode::new(SceneNodeKind::Group).with_children(vec![
            SceneNode::new(SceneNodeKind::Label {
                text: "Scene".to_string(),
            }),
        ]))
    }

    #[test]
    fn render_surface_reports_scene_mode() {
        assert!(RenderSurface::Scene.is_scene());
        assert!(!RenderSurface::Document.is_scene());
    }

    #[test]
    fn render_root_handle_maps_payload_without_losing_surface() {
        let root = RenderRootHandle::new(RenderSurface::Scene, 4_u32);
        let mapped = root.map(|value| value.to_string());

        assert_eq!(mapped.surface, RenderSurface::Scene);
        assert_eq!(mapped.root, "4");
        assert!(mapped.scene.is_none());
        assert!(mapped.is_scene());
    }

    #[test]
    fn scene_render_root_maps_optional_scene_handles() {
        let root = RenderRootHandle::scene(4_u32, Some(5_u32), Some(6_u32));
        let mapped = root.map(|value| value.to_string());

        assert_eq!(mapped.root, "4");
        let scene = mapped.scene.expect("scene metadata should be preserved");
        assert_eq!(scene.lights.as_deref(), Some("5"));
        assert_eq!(scene.geometry.as_deref(), Some("6"));
    }

    #[test]
    fn physical_scene_defaults_match_current_browser_behavior() {
        let params = PhysicalSceneParams::default();

        assert_eq!(params.shadow_dx_per_depth, 1.5);
        assert_eq!(params.shadow_dy_per_depth, 2.0);
        assert_eq!(params.shadow_blur_per_depth, 3.0);
        assert_eq!(params.directional_intensity, 0.8);
        assert_eq!(params.ambient_factor, 0.3);
        assert_eq!(params.bevel_angle, 135.0);
        assert!((params.shadow_opacity() - 0.168).abs() < f64::EPSILON);
    }

    #[test]
    fn render_diff_batch_round_trips_via_serde() {
        let parent = NodeId::new();
        let node = UiNode::new(UiNodeKind::Element {
            tag: "div".to_string(),
            text: None,
            event_ports: Vec::new(),
        });
        let port = EventPortId::new();
        let batch = RenderDiffBatch {
            ops: vec![
                RenderOp::ReplaceRoot(sample_ui_root()),
                RenderOp::InsertChild {
                    parent,
                    index: 1,
                    node: RenderNode::Ui(node.clone()),
                },
                RenderOp::AttachEventPort {
                    id: node.id,
                    port,
                    kind: UiEventKind::Click,
                },
                RenderOp::SetSelectedIndex {
                    id: node.id,
                    index: None,
                },
            ],
        };

        let json = serde_json::to_string(&batch).expect("batch should serialize");
        let restored: RenderDiffBatch =
            serde_json::from_str(&json).expect("batch should deserialize");

        assert_eq!(restored, batch);
    }

    #[test]
    fn move_child_preserves_target_parent_and_index() {
        let parent = NodeId::new();
        let child = NodeId::new();
        let op = RenderOp::MoveChild {
            parent,
            id: child,
            index: 3,
        };

        match op {
            RenderOp::MoveChild {
                parent: p,
                id,
                index,
            } => {
                assert_eq!(p, parent);
                assert_eq!(id, child);
                assert_eq!(index, 3);
            }
            other => panic!("expected MoveChild, got {other:?}"),
        }
    }

    #[test]
    fn attach_and_detach_event_port_keep_port_identity() {
        let id = NodeId::new();
        let port = EventPortId::new();
        let attach = RenderOp::AttachEventPort {
            id,
            port,
            kind: UiEventKind::DoubleClick,
        };
        let detach = RenderOp::DetachEventPort { id, port };

        match attach {
            RenderOp::AttachEventPort {
                id: a_id,
                port: a_port,
                kind,
            } => {
                assert_eq!(a_id, id);
                assert_eq!(a_port, port);
                assert_eq!(kind, UiEventKind::DoubleClick);
            }
            other => panic!("expected AttachEventPort, got {other:?}"),
        }

        match detach {
            RenderOp::DetachEventPort {
                id: d_id,
                port: d_port,
            } => {
                assert_eq!(d_id, id);
                assert_eq!(d_port, port);
            }
            other => panic!("expected DetachEventPort, got {other:?}"),
        }
    }

    #[test]
    fn set_selected_index_allows_none() {
        let op = RenderOp::SetSelectedIndex {
            id: NodeId::new(),
            index: None,
        };

        match op {
            RenderOp::SetSelectedIndex { index, .. } => assert_eq!(index, None),
            other => panic!("expected SetSelectedIndex, got {other:?}"),
        }
    }

    #[test]
    fn replace_root_accepts_ui_and_scene_roots() {
        let ui = RenderOp::ReplaceRoot(sample_ui_root());
        let scene = RenderOp::ReplaceRoot(sample_scene_root());

        match ui {
            RenderOp::ReplaceRoot(RenderRoot::UiTree(_)) => {}
            other => panic!("expected UI root replacement, got {other:?}"),
        }

        match scene {
            RenderOp::ReplaceRoot(RenderRoot::SceneGraph(_)) => {}
            other => panic!("expected scene root replacement, got {other:?}"),
        }
    }
}
