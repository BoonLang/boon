use crate::host_view_preview::HostViewPreviewApp;
use crate::interactive_preview::{InteractivePreview, render_interactive_preview};
use crate::lower::{TextInterpolationUpdateProgram, try_lower_text_interpolation_update};
use boon::platform::browser::kernel::KernelValue;
use boon::zoon::*;
use boon_renderer_zoon::FakeRenderState;
use boon_scene::{RenderRoot, UiEventBatch, UiEventKind};
use std::collections::BTreeMap;

pub struct TextInterpolationUpdatePreview {
    program: TextInterpolationUpdateProgram,
    value: bool,
    app: HostViewPreviewApp,
}

impl TextInterpolationUpdatePreview {
    pub fn new(source: &str) -> Result<Self, String> {
        let program = try_lower_text_interpolation_update(source)?;
        let app =
            HostViewPreviewApp::new(program.host_view.clone(), sinks_for_value(&program, false));
        Ok(Self {
            program,
            value: false,
            app,
        })
    }

    #[must_use]
    pub fn app(&self) -> &HostViewPreviewApp {
        &self.app
    }

    #[must_use]
    pub fn preview_text(&mut self) -> String {
        self.app.preview_text()
    }

    fn toggle(&mut self) {
        self.value = !self.value;
        for (sink, value) in sinks_for_value(&self.program, self.value) {
            self.app.set_sink_value(sink, value);
        }
    }
}

impl InteractivePreview for TextInterpolationUpdatePreview {
    fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool {
        let toggle_port = self
            .app
            .event_port_for_source(self.program.toggle_press_port);
        for event in batch.events {
            if event.kind == UiEventKind::Click && Some(event.target) == toggle_port {
                self.toggle();
                return true;
            }
        }
        false
    }

    fn dispatch_ui_facts(&mut self, _batch: boon_scene::UiFactBatch) -> bool {
        false
    }

    fn render_snapshot(&mut self) -> (RenderRoot, FakeRenderState) {
        let (root, state) = self.app.render_snapshot();
        (RenderRoot::UiTree(root), state)
    }
}

pub fn render_text_interpolation_update_preview(
    preview: TextInterpolationUpdatePreview,
) -> impl Element {
    render_interactive_preview(preview)
}

fn sinks_for_value(
    program: &TextInterpolationUpdateProgram,
    value: bool,
) -> BTreeMap<crate::ir::SinkPortId, KernelValue> {
    let value_text = if value { "True" } else { "False" };
    BTreeMap::from([
        (
            program.button_label_sink,
            KernelValue::from(format!("Toggle (value: {value_text})")),
        ),
        (
            program.label_sink,
            KernelValue::from(format!("Label shows: {value_text}")),
        ),
        (
            program.while_sink,
            KernelValue::from(format!("WHILE says: {value_text}")),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use boon_scene::{UiEvent, UiEventKind};

    #[test]
    fn text_interpolation_preview_updates_all_labels_on_toggle() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/text_interpolation_update/text_interpolation_update.bn"
        );
        let mut preview =
            TextInterpolationUpdatePreview::new(source).expect("text_interpolation preview");
        assert_eq!(
            preview.preview_text(),
            "Toggle (value: False)Label shows: FalseWHILE says: False"
        );

        let _ = preview.render_snapshot();
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(preview.program.toggle_press_port)
                    .expect("toggle port"),
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        assert_eq!(
            preview.preview_text(),
            "Toggle (value: True)Label shows: TrueWHILE says: True"
        );
    }
}
