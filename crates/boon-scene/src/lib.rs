use serde::{Deserialize, Serialize};
use ulid::Ulid;

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
