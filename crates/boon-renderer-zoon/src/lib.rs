pub use boon_scene::{EventPortId, NodeId, RenderRoot, SceneDiff, UiEventBatch, UiFactBatch};
pub use zoon;

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
