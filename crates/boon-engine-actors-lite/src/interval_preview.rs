use crate::host_view_preview::HostViewPreviewApp;
use crate::ids::ActorId;
use crate::ir_executor::IrExecutor;
use crate::lower::{IntervalProgram, try_lower_interval, try_lower_interval_hold};
use crate::preview_runtime::PreviewRuntime;
use crate::runtime::ActorKind;
use boon::platform::browser::kernel::KernelValue;
use boon::zoon::*;
use boon_renderer_zoon::FakeRenderState;
use boon_scene::{UiEventBatch, UiEventKind, UiNode};

pub struct IntervalPreview {
    runtime: PreviewRuntime,
    tick_actor: ActorId,
    program: IntervalProgram,
    executor: IrExecutor,
    app: HostViewPreviewApp,
}

impl IntervalPreview {
    pub fn new(source: &str) -> Result<Self, String> {
        let program = try_lower_interval(source).or_else(|_| try_lower_interval_hold(source))?;
        let mut runtime = PreviewRuntime::new();
        let tick_actor = runtime.alloc_actor(ActorKind::SourcePort);
        let executor = IrExecutor::new(program.ir.clone())?;
        let app =
            HostViewPreviewApp::new(program.host_view.clone(), executor.sink_values().clone());

        Ok(Self {
            runtime,
            tick_actor,
            program,
            executor,
            app,
        })
    }

    pub fn dispatch_ui_events(&mut self, batch: UiEventBatch) {
        let tick_event_port = self.app.event_port_for_source(self.program.tick_port);
        let mut saw_tick = false;
        for event in batch.events {
            if Some(event.target) == tick_event_port && matches!(event.kind, UiEventKind::Custom(_))
            {
                saw_tick = true;
            }
        }

        if !saw_tick {
            return;
        }

        let messages = self.runtime.dispatch_pulse(
            self.tick_actor,
            self.program.tick_port,
            KernelValue::from("tick"),
        );
        self.executor
            .apply_messages(&messages)
            .expect("interval IR should execute");
        for (sink, value) in self.executor.sink_values() {
            self.app.set_sink_value(*sink, value.clone());
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
    pub fn app(&self) -> &HostViewPreviewApp {
        &self.app
    }
}

pub fn render_interval_preview(preview: IntervalPreview) -> impl Element {
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

    fn tick(preview: &mut IntervalPreview) {
        let _ = preview.render_root();
        let tick_target = preview
            .app()
            .event_port_for_source(preview.program.tick_port)
            .expect("tick port");
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: tick_target,
                kind: UiEventKind::Custom(format!("timer:{}", preview.program.interval_ms)),
                payload: None,
            }],
        });
    }

    #[test]
    fn interval_preview_counts_ticks() {
        let source = include_str!("../../../playground/frontend/src/examples/interval/interval.bn");
        let mut preview = IntervalPreview::new(source).expect("interval preview");

        assert_eq!(preview.preview_text(), "");
        tick(&mut preview);
        assert_eq!(preview.preview_text(), "1");
        tick(&mut preview);
        tick(&mut preview);
        assert_eq!(preview.preview_text(), "3");
    }

    #[test]
    fn interval_hold_preview_counts_ticks() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/interval_hold/interval_hold.bn"
        );
        let mut preview = IntervalPreview::new(source).expect("interval_hold preview");

        assert_eq!(preview.preview_text(), "");
        tick(&mut preview);
        assert_eq!(preview.preview_text(), "1");
        tick(&mut preview);
        tick(&mut preview);
        assert_eq!(preview.preview_text(), "3");
    }
}
