use crate::host_view_preview::{HostViewPreviewApp, render_static_host_view};
use crate::lower::{LayersProgram, try_lower_layers};
use boon::zoon::*;

#[derive(Debug)]
pub struct LayersPreview {
    app: HostViewPreviewApp,
}

impl LayersPreview {
    pub fn new(source: &str) -> Result<Self, String> {
        let LayersProgram {
            host_view,
            sink_values,
        } = try_lower_layers(source)?;
        Ok(Self {
            app: HostViewPreviewApp::new(host_view, sink_values),
        })
    }

    #[must_use]
    pub fn preview_text(&mut self) -> String {
        self.app.preview_text()
    }
}

pub fn render_layers_preview(preview: LayersPreview) -> impl Element {
    render_static_host_view(preview.app)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layers_preview_renders_expected_labels() {
        let source = include_str!("../../../playground/frontend/src/examples/layers/layers.bn");
        let mut preview = LayersPreview::new(source).expect("layers preview");
        assert_eq!(preview.preview_text(), "Red CardGreen CardBlue Card");
    }
}
