use crate::host_view_preview::{HostViewPreviewApp, render_static_host_view};
use crate::lower::{StaticProgram, lower_program};
use boon::zoon::*;

#[derive(Debug)]
pub struct StaticPreview {
    app: HostViewPreviewApp,
}

impl StaticPreview {
    pub fn new(source: &str) -> Result<Self, String> {
        Ok(Self::from_program(
            lower_program(source)?.into_static_program()?,
        ))
    }

    pub fn from_program(program: StaticProgram) -> Self {
        let StaticProgram {
            host_view,
            sink_values,
        } = program;
        Self {
            app: HostViewPreviewApp::new(host_view, sink_values),
        }
    }

    #[must_use]
    pub fn preview_text(&mut self) -> String {
        self.app.preview_text()
    }
}

pub fn render_static_preview(preview: StaticPreview) -> impl Element {
    render_static_host_view(preview.app)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowers_minimal_and_hello_world_static_examples() {
        let minimal = include_str!("../../../playground/frontend/src/examples/minimal/minimal.bn");
        let hello_world =
            include_str!("../../../playground/frontend/src/examples/hello_world/hello_world.bn");

        let mut minimal_preview = StaticPreview::new(minimal).expect("minimal preview");
        let mut hello_preview = StaticPreview::new(hello_world).expect("hello preview");

        assert_eq!(minimal_preview.preview_text(), "123");
        assert_eq!(hello_preview.preview_text(), "Hello world!");
    }

    #[test]
    fn rejects_non_static_document_roots() {
        let source = r#"
document: Document/new(root: value)
value: 123
"#;

        let error = StaticPreview::new(source).expect_err("should reject alias root");
        assert!(error.contains("static subset"));
    }
}
