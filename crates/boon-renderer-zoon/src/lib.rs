pub use boon_scene::{
    EventPortId, NodeId, RenderRoot, RenderRootHandle, RenderSurface, SceneDiff, UiEventBatch,
    UiFactBatch,
};
pub use zoon;
use zoon::Unify;

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

#[cfg(test)]
mod tests {
    use super::{RenderMode, RendererCapabilities, render_mode_for_surface};
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
}
