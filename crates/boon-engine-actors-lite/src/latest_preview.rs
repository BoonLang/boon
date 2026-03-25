use crate::host_view_preview::HostViewPreviewApp;
use crate::interactive_preview::{InteractivePreview, render_interactive_preview};
use crate::lower::{LatestProgram, try_lower_latest};
use boon::platform::browser::kernel::KernelValue;
use boon::zoon::*;
use boon_renderer_zoon::FakeRenderState;
use boon_scene::{RenderRoot, UiEventBatch, UiEventKind};

pub struct LatestPreview {
    program: LatestProgram,
    current_value: i64,
    app: HostViewPreviewApp,
}

impl LatestPreview {
    pub fn new(source: &str) -> Result<Self, String> {
        let program = try_lower_latest(source)?;
        let mut app =
            HostViewPreviewApp::new(program.host_view.clone(), initial_sinks(&program, 3));
        app.set_sink_value(program.value_sink, KernelValue::from("3"));
        Ok(Self {
            program,
            current_value: 3,
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

    fn set_value(&mut self, value: i64) -> bool {
        if self.current_value == value {
            return false;
        }
        self.current_value = value;
        self.app.set_sink_value(
            self.program.value_sink,
            KernelValue::from(value.to_string()),
        );
        self.app.set_sink_value(
            self.program.sum_sink,
            KernelValue::from(format!("Sum: {value}")),
        );
        true
    }
}

impl InteractivePreview for LatestPreview {
    fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool {
        let first = self
            .app
            .event_port_for_source(self.program.send_press_ports[0]);
        let second = self
            .app
            .event_port_for_source(self.program.send_press_ports[1]);

        for event in batch.events {
            if event.kind != UiEventKind::Click {
                continue;
            }
            if Some(event.target) == first {
                return self.set_value(1);
            }
            if Some(event.target) == second {
                return self.set_value(2);
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

pub fn render_latest_preview(preview: LatestPreview) -> impl Element {
    render_interactive_preview(preview)
}

fn initial_sinks(
    program: &LatestProgram,
    value: i64,
) -> std::collections::BTreeMap<crate::ir::SinkPortId, KernelValue> {
    std::collections::BTreeMap::from([
        (program.value_sink, KernelValue::from(value.to_string())),
        (program.sum_sink, KernelValue::from(format!("Sum: {value}"))),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use boon_scene::{UiEvent, UiEventKind};

    #[test]
    fn latest_preview_updates_to_most_recent_button_value() {
        let source = include_str!("../../../playground/frontend/src/examples/latest/latest.bn");
        let mut preview = LatestPreview::new(source).expect("latest preview");
        assert_eq!(preview.preview_text(), "Send 1Send 23Sum: 3");

        let _ = preview.render_snapshot();
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(preview.program.send_press_ports[0])
                    .expect("send 1 port"),
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        assert_eq!(preview.preview_text(), "Send 1Send 21Sum: 1");

        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(preview.program.send_press_ports[1])
                    .expect("send 2 port"),
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        assert_eq!(preview.preview_text(), "Send 1Send 22Sum: 2");
    }
}
