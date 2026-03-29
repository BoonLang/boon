use crate::bridge::HostInput;
use crate::ids::ActorId;
use crate::ir_executor::IrExecutor;
use crate::lower::{CircleDrawerProgram, try_lower_circle_drawer};
use crate::preview_runtime::PreviewRuntime;
use boon::platform::browser::kernel::KernelValue;
use boon::zoon::*;
use boon_renderer_zoon::FakeRenderState;
use boon_scene::{UiEventBatch, UiEventKind, UiNode};
use std::collections::BTreeMap;

pub struct CircleDrawerPreview {
    runtime: PreviewRuntime,
    canvas_actor: ActorId,
    undo_actor: ActorId,
    program: CircleDrawerProgram,
    executor: IrExecutor,
    app: crate::host_view_preview::HostViewPreviewApp,
}

impl CircleDrawerPreview {
    pub fn new(source: &str) -> Result<Self, String> {
        let program = try_lower_circle_drawer(source)?;
        let mut runtime = PreviewRuntime::new();
        let canvas_actor = runtime.alloc_actor();
        let undo_actor = runtime.alloc_actor();
        let executor = IrExecutor::new(program.ir.clone())?;
        let app = crate::host_view_preview::HostViewPreviewApp::new(
            program.host_view.clone(),
            executor.sink_values(),
        );
        Ok(Self {
            runtime,
            canvas_actor,
            undo_actor,
            program,
            executor,
            app,
        })
    }

    pub fn dispatch_ui_events(&mut self, batch: UiEventBatch) {
        let _ = self.app.render_root();
        let canvas_port = self
            .app
            .event_port_for_source(self.program.canvas_click_port);
        let undo_port = self.app.event_port_for_source(self.program.undo_press_port);
        let mut inputs = Vec::new();

        for (seq, event) in batch.events.into_iter().enumerate() {
            if Some(event.target) == canvas_port && matches!(event.kind, UiEventKind::Click) {
                inputs.push(HostInput::Pulse {
                    actor: self.canvas_actor,
                    port: self.program.canvas_click_port,
                    value: parse_canvas_click_payload(event.payload.as_deref()),
                    seq: self.runtime.causal_seq(seq as u32),
                });
                continue;
            }
            if Some(event.target) == undo_port && matches!(event.kind, UiEventKind::Click) {
                inputs.push(HostInput::Pulse {
                    actor: self.undo_actor,
                    port: self.program.undo_press_port,
                    value: KernelValue::from("press"),
                    seq: self.runtime.causal_seq(seq as u32),
                });
            }
        }

        if inputs.is_empty() {
            return;
        }

        let (runtime, executor, app) = (&mut self.runtime, &mut self.executor, &mut self.app);
        runtime.dispatch_inputs_batches(inputs.as_slice(), |messages| {
            executor
                .apply_pure_messages_owned(messages.drain(..))
                .expect("circle drawer IR should execute");
        });
        for (sink, value) in executor.sink_values() {
            app.set_sink_value(sink, value);
        }
    }

    #[must_use]
    pub fn render_root(&mut self) -> UiNode {
        self.app.render_root()
    }

    fn render_snapshot(&mut self) -> (UiNode, FakeRenderState) {
        self.app.render_snapshot()
    }

    #[must_use]
    pub fn preview_text(&mut self) -> String {
        self.app.preview_text()
    }

    #[must_use]
    #[cfg(test)]
    pub(crate) fn app(&self) -> &crate::host_view_preview::HostViewPreviewApp {
        &self.app
    }
}

fn parse_canvas_click_payload(payload: Option<&str>) -> KernelValue {
    let Some(payload) = payload else {
        return KernelValue::Skip;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return KernelValue::Skip;
    };
    let Some(object) = value.as_object() else {
        return KernelValue::Skip;
    };
    let mut fields = BTreeMap::new();
    for (name, value) in object {
        if let Some(number) = value.as_f64() {
            fields.insert(name.clone(), KernelValue::from(number));
        }
    }
    if fields.is_empty() {
        KernelValue::Skip
    } else {
        KernelValue::Object(fields)
    }
}

pub fn render_circle_drawer_preview(preview: CircleDrawerPreview) -> impl Element {
    crate::preview_shell::render_preview_shell(
        preview,
        |preview, batch| preview.dispatch_ui_events(batch),
        |preview| preview.render_snapshot(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use boon_scene::UiEvent;

    #[test]
    fn circle_drawer_preview_tracks_clicks_and_undo() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/circle_drawer/circle_drawer.bn"
        );
        let mut preview = CircleDrawerPreview::new(source).expect("circle_drawer preview");

        assert!(preview.preview_text().contains("Circle Drawer"));
        assert!(preview.preview_text().contains("Circles: 0"));

        let _ = preview.render_root();
        let canvas_target = preview
            .app()
            .event_port_for_source(preview.program.canvas_click_port)
            .expect("canvas port");
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: canvas_target,
                kind: UiEventKind::Click,
                payload: Some("{\"x\":120,\"y\":80}".to_string()),
            }],
        });
        assert!(preview.preview_text().contains("Circles: 1"));
        let root = preview.render_root();
        let canvas = &root.children[0].children[2];
        assert_eq!(canvas.children.len(), 1);
        assert_eq!(canvas.children[0].children.len(), 1);

        let undo_target = preview
            .app()
            .event_port_for_source(preview.program.undo_press_port)
            .expect("undo port");
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: undo_target,
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        assert!(preview.preview_text().contains("Circles: 0"));
        let root = preview.render_root();
        let canvas = &root.children[0].children[2];
        assert_eq!(canvas.children.len(), 1);
        assert!(canvas.children[0].children.is_empty());
    }
}
