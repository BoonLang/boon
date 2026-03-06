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
    use super::{PhysicalSceneParams, RenderRootHandle, RenderSurface};

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
}
