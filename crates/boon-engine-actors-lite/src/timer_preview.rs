use crate::bridge::{HostInput, HostSnapshot};
use crate::ids::ActorId;
use crate::input_form_runtime::{FormInputBinding, FormInputEvent};
use crate::ir_executor::IrExecutor;
use crate::lower::{TimerProgram, try_lower_timer};
use crate::preview_runtime::PreviewRuntime;
use crate::runtime::ActorKind;
use crate::validated_form_runtime::ValidatedFormRuntime;
use boon::platform::browser::kernel::KernelValue;
use boon::zoon::*;
use boon_renderer_zoon::FakeRenderState;
use boon_scene::{UiEventBatch, UiEventKind, UiNode};

pub struct TimerPreview {
    runtime: PreviewRuntime,
    duration_actor: ActorId,
    reset_actor: ActorId,
    tick_actor: ActorId,
    program: TimerProgram,
    executor: IrExecutor,
    form: ValidatedFormRuntime<1>,
}

impl TimerPreview {
    pub fn new(source: &str) -> Result<Self, String> {
        let program = try_lower_timer(source)?;
        let mut runtime = PreviewRuntime::new();
        let duration_actor = runtime.alloc_actor(ActorKind::SourcePort);
        let reset_actor = runtime.alloc_actor(ActorKind::SourcePort);
        let tick_actor = runtime.alloc_actor(ActorKind::SourcePort);
        let executor = IrExecutor::new(program.ir.clone())?;
        let form = ValidatedFormRuntime::new(
            program.host_view.clone(),
            executor.sink_values().clone(),
            [FormInputBinding {
                change_port: program.duration_change_port,
                key_down_port: None,
            }],
            [program.reset_press_port],
        );

        Ok(Self {
            runtime,
            duration_actor,
            reset_actor,
            tick_actor,
            program,
            executor,
            form,
        })
    }

    pub fn dispatch_ui_events(&mut self, batch: UiEventBatch) {
        let tick_event_port = self
            .form
            .app()
            .event_port_for_source(self.program.tick_port);
        let dispatch = self.form.dispatch_ui_events(batch.clone());
        let mut inputs = dispatch
            .input_events
            .iter()
            .enumerate()
            .filter_map(|(seq, event)| match event {
                FormInputEvent::Changed { index: 0 } => Some(HostInput::Pulse {
                    actor: self.duration_actor,
                    port: self.program.duration_change_port,
                    value: KernelValue::from(self.form.input(0).to_string()),
                    seq: self.runtime.causal_seq(seq as u32),
                }),
                FormInputEvent::Changed { .. } | FormInputEvent::KeyDown { .. } => None,
            })
            .collect::<Vec<_>>();

        let mut next_seq = inputs.len() as u32;
        for port in dispatch.clicked_ports {
            if port == self.program.reset_press_port {
                inputs.push(HostInput::Pulse {
                    actor: self.reset_actor,
                    port,
                    value: KernelValue::from("press"),
                    seq: self.runtime.causal_seq(next_seq),
                });
                next_seq += 1;
            }
        }

        for event in &batch.events {
            if Some(event.target) == tick_event_port && matches!(event.kind, UiEventKind::Custom(_))
            {
                inputs.push(HostInput::Pulse {
                    actor: self.tick_actor,
                    port: self.program.tick_port,
                    value: KernelValue::from("tick"),
                    seq: self.runtime.causal_seq(next_seq),
                });
                next_seq += 1;
            }
        }

        if inputs.is_empty() {
            return;
        }

        let messages = self.runtime.dispatch_snapshot(HostSnapshot::new(inputs));
        self.executor
            .apply_messages(&messages)
            .expect("timer IR should execute");
        for (sink, value) in self.executor.sink_values() {
            self.form.set_sink_value(*sink, value.clone());
        }
    }

    #[must_use]
    pub fn render_root(&mut self) -> UiNode {
        self.form.render_root()
    }

    fn render_snapshot(&mut self) -> (UiNode, FakeRenderState) {
        self.form.render_snapshot()
    }

    #[must_use]
    pub fn preview_text(&mut self) -> String {
        self.form.preview_text()
    }

    #[must_use]
    pub fn app(&self) -> &crate::host_view_preview::HostViewPreviewApp {
        self.form.app()
    }
}

pub fn render_timer_preview(preview: TimerPreview) -> impl Element {
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
    fn timer_preview_ticks_resets_and_respects_duration() {
        let source = include_str!("../../../playground/frontend/src/examples/timer/timer.bn");
        let mut preview = TimerPreview::new(source).expect("timer preview");

        assert!(preview.preview_text().contains("Timer"));
        assert!(preview.preview_text().contains("15s"));

        let _ = preview.render_root();
        let tick_target = preview
            .app()
            .event_port_for_source(preview.program.tick_port)
            .expect("tick port");
        for _ in 0..5 {
            preview.dispatch_ui_events(UiEventBatch {
                events: vec![UiEvent {
                    target: tick_target,
                    kind: UiEventKind::Custom("timer:100".to_string()),
                    payload: None,
                }],
            });
        }
        assert!(preview.preview_text().contains("0.5s"));
        assert!(preview.preview_text().contains("3%"));

        let slider_target = preview
            .app()
            .event_port_for_source(preview.program.duration_change_port)
            .expect("slider port");
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: slider_target,
                kind: UiEventKind::Input,
                payload: Some("2".to_string()),
            }],
        });
        assert!(preview.preview_text().contains("2s"));
        assert!(preview.preview_text().contains("25%"));

        for _ in 0..20 {
            preview.dispatch_ui_events(UiEventBatch {
                events: vec![UiEvent {
                    target: tick_target,
                    kind: UiEventKind::Custom("timer:100".to_string()),
                    payload: None,
                }],
            });
        }
        assert!(preview.preview_text().contains("100%"));

        let reset_target = preview
            .app()
            .event_port_for_source(preview.program.reset_press_port)
            .expect("reset port");
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: reset_target,
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        assert!(preview.preview_text().contains("0s"));
        assert!(!preview.preview_text().contains("100%"));
    }
}
