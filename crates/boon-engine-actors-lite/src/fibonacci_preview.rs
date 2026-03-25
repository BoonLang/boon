use crate::host_view_preview::{HostViewPreviewApp, render_static_host_view};
use crate::lower::{FibonacciProgram, try_lower_fibonacci};
use boon::zoon::*;

#[derive(Debug)]
pub struct FibonacciPreview {
    app: HostViewPreviewApp,
}

impl FibonacciPreview {
    pub fn new(source: &str) -> Result<Self, String> {
        let FibonacciProgram {
            host_view,
            sink_values,
        } = try_lower_fibonacci(source)?;
        Ok(Self {
            app: HostViewPreviewApp::new(host_view, sink_values),
        })
    }

    #[must_use]
    pub fn preview_text(&mut self) -> String {
        self.app.preview_text()
    }
}

pub fn render_fibonacci_preview(preview: FibonacciPreview) -> impl Element {
    render_static_host_view(preview.app)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fibonacci_preview_renders_expected_message() {
        let source =
            include_str!("../../../playground/frontend/src/examples/fibonacci/fibonacci.bn");
        let mut preview = FibonacciPreview::new(source).expect("fibonacci preview");
        assert_eq!(preview.preview_text(), "10. Fibonacci number is 55");
    }
}
