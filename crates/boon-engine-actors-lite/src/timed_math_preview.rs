use crate::bridge::HostInput;
use crate::host_view_preview::{
    HostViewPreviewApp, InteractiveHostViewModel, render_interactive_host_view,
};
use crate::ids::ActorId;
use crate::ir::SinkPortId;
use crate::ir_executor::IrExecutor;
use crate::lower::{ThenProgram, WhenProgram, WhileProgram, lower_program};
use crate::preview_runtime::PreviewRuntime;
use boon::platform::browser::kernel::KernelValue;
use boon::zoon::*;
use boon_scene::{UiEvent, UiEventBatch, UiEventKind};

pub struct ThenPreview {
    program: ThenProgram,
    runtime: PreviewRuntime,
    host_actor: ActorId,
    executor: IrExecutor,
    app: HostViewPreviewApp,
}

impl ThenPreview {
    pub fn new(source: &str) -> Result<Self, String> {
        Self::from_program(lower_program(source)?.into_then_program()?)
    }

    pub fn from_program(program: ThenProgram) -> Result<Self, String> {
        let mut runtime = PreviewRuntime::new();
        let host_actor = runtime.alloc_actor();
        let executor = IrExecutor::new(program.ir.clone())?;
        let app = HostViewPreviewApp::new(program.host_view.clone(), executor.sink_values());
        Ok(Self {
            program,
            runtime,
            host_actor,
            executor,
            app,
        })
    }

    #[must_use]
    pub(crate) fn app(&self) -> &HostViewPreviewApp {
        &self.app
    }

    #[must_use]
    pub fn preview_text(&mut self) -> String {
        self.app.preview_text()
    }

    fn refresh_sink_values(&mut self) {
        sync_sink_values(
            &mut self.app,
            &self.executor,
            &[
                self.program.input_a_sink,
                self.program.input_b_sink,
                self.program.result_sink,
            ],
        );
    }

    fn apply_messages(&mut self, inputs: Vec<HostInput>) -> bool {
        if inputs.is_empty() {
            return false;
        }
        let Self {
            runtime, executor, ..
        } = self;
        runtime.dispatch_inputs_batches(inputs.as_slice(), |messages| {
            executor
                .apply_pure_messages_owned(messages.drain(..))
                .expect("then IR should execute");
        });
        self.refresh_sink_values();
        true
    }
}

impl InteractiveHostViewModel for ThenPreview {
    fn app_mut(&mut self) -> &mut HostViewPreviewApp {
        &mut self.app
    }

    fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool {
        let ports = TimedMathPorts {
            input_a_tick_port: self.program.input_a_tick_port,
            input_b_tick_port: self.program.input_b_tick_port,
            addition_press_port: Some(self.program.addition_press_port),
            subtraction_press_port: None,
        };
        self.apply_messages(timed_math_inputs_from_events(
            self.app(),
            self.host_actor,
            &self.runtime,
            ports,
            batch.events,
        ))
    }
}

pub struct WhenPreview {
    program: WhenProgram,
    runtime: PreviewRuntime,
    host_actor: ActorId,
    executor: IrExecutor,
    app: HostViewPreviewApp,
}

impl WhenPreview {
    pub fn new(source: &str) -> Result<Self, String> {
        Self::from_program(lower_program(source)?.into_when_program()?)
    }

    pub fn from_program(program: WhenProgram) -> Result<Self, String> {
        let mut runtime = PreviewRuntime::new();
        let host_actor = runtime.alloc_actor();
        let executor = IrExecutor::new(program.ir.clone())?;
        let app = HostViewPreviewApp::new(program.host_view.clone(), executor.sink_values());
        Ok(Self {
            program,
            runtime,
            host_actor,
            executor,
            app,
        })
    }

    #[must_use]
    pub(crate) fn app(&self) -> &HostViewPreviewApp {
        &self.app
    }

    #[must_use]
    pub fn preview_text(&mut self) -> String {
        self.app.preview_text()
    }

    fn refresh_sink_values(&mut self) {
        sync_sink_values(
            &mut self.app,
            &self.executor,
            &[
                self.program.input_a_sink,
                self.program.input_b_sink,
                self.program.result_sink,
            ],
        );
    }

    fn apply_messages(&mut self, inputs: Vec<HostInput>) -> bool {
        if inputs.is_empty() {
            return false;
        }
        let Self {
            runtime, executor, ..
        } = self;
        runtime.dispatch_inputs_batches(inputs.as_slice(), |messages| {
            executor
                .apply_pure_messages_owned(messages.drain(..))
                .expect("when IR should execute");
        });
        self.refresh_sink_values();
        true
    }
}

impl InteractiveHostViewModel for WhenPreview {
    fn app_mut(&mut self) -> &mut HostViewPreviewApp {
        &mut self.app
    }

    fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool {
        let ports = TimedMathPorts {
            input_a_tick_port: self.program.input_a_tick_port,
            input_b_tick_port: self.program.input_b_tick_port,
            addition_press_port: Some(self.program.addition_press_port),
            subtraction_press_port: Some(self.program.subtraction_press_port),
        };
        self.apply_messages(timed_math_inputs_from_events(
            self.app(),
            self.host_actor,
            &self.runtime,
            ports,
            batch.events,
        ))
    }
}

pub struct WhilePreview {
    program: WhileProgram,
    runtime: PreviewRuntime,
    host_actor: ActorId,
    executor: IrExecutor,
    app: HostViewPreviewApp,
}

impl WhilePreview {
    pub fn new(source: &str) -> Result<Self, String> {
        Self::from_program(lower_program(source)?.into_while_program()?)
    }

    pub fn from_program(program: WhileProgram) -> Result<Self, String> {
        let mut runtime = PreviewRuntime::new();
        let host_actor = runtime.alloc_actor();
        let executor = IrExecutor::new(program.ir.clone())?;
        let app = HostViewPreviewApp::new(program.host_view.clone(), executor.sink_values());
        Ok(Self {
            program,
            runtime,
            host_actor,
            executor,
            app,
        })
    }

    #[must_use]
    pub(crate) fn app(&self) -> &HostViewPreviewApp {
        &self.app
    }

    #[must_use]
    pub fn preview_text(&mut self) -> String {
        self.app.preview_text()
    }

    fn refresh_sink_values(&mut self) {
        sync_sink_values(
            &mut self.app,
            &self.executor,
            &[
                self.program.input_a_sink,
                self.program.input_b_sink,
                self.program.result_sink,
            ],
        );
    }

    fn apply_messages(&mut self, inputs: Vec<HostInput>) -> bool {
        if inputs.is_empty() {
            return false;
        }
        let Self {
            runtime, executor, ..
        } = self;
        runtime.dispatch_inputs_batches(inputs.as_slice(), |messages| {
            executor
                .apply_pure_messages_owned(messages.drain(..))
                .expect("while IR should execute");
        });
        self.refresh_sink_values();
        true
    }
}

impl InteractiveHostViewModel for WhilePreview {
    fn app_mut(&mut self) -> &mut HostViewPreviewApp {
        &mut self.app
    }

    fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool {
        let ports = TimedMathPorts {
            input_a_tick_port: self.program.input_a_tick_port,
            input_b_tick_port: self.program.input_b_tick_port,
            addition_press_port: Some(self.program.addition_press_port),
            subtraction_press_port: Some(self.program.subtraction_press_port),
        };
        self.apply_messages(timed_math_inputs_from_events(
            self.app(),
            self.host_actor,
            &self.runtime,
            ports,
            batch.events,
        ))
    }
}

#[derive(Clone, Copy)]
struct TimedMathPorts {
    input_a_tick_port: crate::ir::SourcePortId,
    input_b_tick_port: crate::ir::SourcePortId,
    addition_press_port: Option<crate::ir::SourcePortId>,
    subtraction_press_port: Option<crate::ir::SourcePortId>,
}

fn timed_math_inputs_from_events(
    app: &HostViewPreviewApp,
    host_actor: ActorId,
    runtime: &PreviewRuntime,
    ports: TimedMathPorts,
    events: Vec<UiEvent>,
) -> Vec<HostInput> {
    let input_a_tick = app.event_port_for_source(ports.input_a_tick_port);
    let input_b_tick = app.event_port_for_source(ports.input_b_tick_port);
    let addition = ports
        .addition_press_port
        .and_then(|port| app.event_port_for_source(port))
        .map(|event_port| {
            (
                event_port,
                ports.addition_press_port.expect("addition port"),
            )
        });
    let subtraction = ports
        .subtraction_press_port
        .and_then(|port| app.event_port_for_source(port))
        .map(|event_port| {
            (
                event_port,
                ports.subtraction_press_port.expect("subtraction port"),
            )
        });

    events
        .into_iter()
        .enumerate()
        .filter_map(|(index, event)| {
            if Some(event.target) == input_a_tick && matches!(event.kind, UiEventKind::Custom(_)) {
                return Some(HostInput::Pulse {
                    actor: host_actor,
                    port: ports.input_a_tick_port,
                    value: KernelValue::from("tick"),
                    seq: runtime.causal_seq(index as u32),
                });
            }
            if Some(event.target) == input_b_tick && matches!(event.kind, UiEventKind::Custom(_)) {
                return Some(HostInput::Pulse {
                    actor: host_actor,
                    port: ports.input_b_tick_port,
                    value: KernelValue::from("tick"),
                    seq: runtime.causal_seq(index as u32),
                });
            }
            if let Some((event_port, port)) = addition
                && Some(event.target) == Some(event_port)
                && event.kind == UiEventKind::Click
            {
                return Some(HostInput::Pulse {
                    actor: host_actor,
                    port,
                    value: KernelValue::from("press"),
                    seq: runtime.causal_seq(index as u32),
                });
            }
            if let Some((event_port, port)) = subtraction
                && Some(event.target) == Some(event_port)
                && event.kind == UiEventKind::Click
            {
                return Some(HostInput::Pulse {
                    actor: host_actor,
                    port,
                    value: KernelValue::from("press"),
                    seq: runtime.causal_seq(index as u32),
                });
            }
            None
        })
        .collect()
}

fn sync_sink_values(app: &mut HostViewPreviewApp, executor: &IrExecutor, sinks: &[SinkPortId]) {
    for sink in sinks {
        app.set_sink_value(
            *sink,
            executor
                .sink_value(*sink)
                .cloned()
                .unwrap_or(KernelValue::Skip),
        );
    }
}

pub fn render_then_preview(preview: ThenPreview) -> impl Element {
    render_interactive_host_view(preview)
}

pub fn render_when_preview(preview: WhenPreview) -> impl Element {
    render_interactive_host_view(preview)
}

pub fn render_while_preview(preview: WhilePreview) -> impl Element {
    render_interactive_host_view(preview)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn click(target: boon_scene::EventPortId) -> UiEventBatch {
        UiEventBatch {
            events: vec![UiEvent {
                target,
                kind: UiEventKind::Click,
                payload: None,
            }],
        }
    }

    fn timer(target: boon_scene::EventPortId, interval_ms: u32) -> UiEvent {
        UiEvent {
            target,
            kind: UiEventKind::Custom(format!("timer:{interval_ms}")),
            payload: None,
        }
    }

    fn one_second_tick(
        fast_target: boon_scene::EventPortId,
        slow_target: boon_scene::EventPortId,
    ) -> UiEventBatch {
        UiEventBatch {
            events: vec![
                timer(fast_target, 500),
                timer(fast_target, 500),
                timer(slow_target, 1000),
            ],
        }
    }

    fn half_then_one_second_ticks(
        fast_target: boon_scene::EventPortId,
        slow_target: boon_scene::EventPortId,
    ) -> [UiEventBatch; 2] {
        [
            UiEventBatch {
                events: vec![timer(fast_target, 500)],
            },
            UiEventBatch {
                events: vec![timer(fast_target, 500), timer(slow_target, 1000)],
            },
        ]
    }

    #[test]
    fn then_preview_keeps_sum_stable_until_next_press() {
        let source = include_str!("../../../playground/frontend/src/examples/then/then.bn");
        let mut preview = ThenPreview::new(source).expect("then preview");
        assert_eq!(preview.preview_text(), "A: 0B: 0A + B");

        let _ = preview.render_snapshot();
        let input_a_tick = preview
            .app()
            .event_port_for_source(preview.program.input_a_tick_port)
            .expect("input_a tick");
        let input_b_tick = preview
            .app()
            .event_port_for_source(preview.program.input_b_tick_port)
            .expect("input_b tick");
        let addition = preview
            .app()
            .event_port_for_source(preview.program.addition_press_port)
            .expect("addition");

        for batch in half_then_one_second_ticks(input_a_tick, input_b_tick) {
            preview.dispatch_ui_events(batch);
        }
        assert_eq!(preview.preview_text(), "A: 2B: 10A + B");

        preview.dispatch_ui_events(click(addition));
        assert_eq!(preview.preview_text(), "A: 2B: 10A + B12");

        for batch in half_then_one_second_ticks(input_a_tick, input_b_tick) {
            preview.dispatch_ui_events(batch);
        }
        assert_eq!(preview.preview_text(), "A: 4B: 20A + B12");

        preview.dispatch_ui_events(click(addition));
        assert_eq!(preview.preview_text(), "A: 4B: 20A + B24");
    }

    #[test]
    fn when_preview_keeps_result_stable_until_operation_changes() {
        let source = include_str!("../../../playground/frontend/src/examples/when/when.bn");
        let mut preview = WhenPreview::new(source).expect("when preview");
        assert_eq!(preview.preview_text(), "A: 0B: 0A + BA - B");

        let _ = preview.render_snapshot();
        let input_a_tick = preview
            .app()
            .event_port_for_source(preview.program.input_a_tick_port)
            .expect("input_a tick");
        let input_b_tick = preview
            .app()
            .event_port_for_source(preview.program.input_b_tick_port)
            .expect("input_b tick");
        let addition = preview
            .app()
            .event_port_for_source(preview.program.addition_press_port)
            .expect("addition");
        let subtraction = preview
            .app()
            .event_port_for_source(preview.program.subtraction_press_port)
            .expect("subtraction");

        for batch in half_then_one_second_ticks(input_a_tick, input_b_tick) {
            preview.dispatch_ui_events(batch);
        }
        preview.dispatch_ui_events(click(addition));
        assert_eq!(preview.preview_text(), "A: 2B: 10A + BA - B12");

        for batch in half_then_one_second_ticks(input_a_tick, input_b_tick) {
            preview.dispatch_ui_events(batch);
        }
        assert_eq!(preview.preview_text(), "A: 4B: 20A + BA - B12");

        preview.dispatch_ui_events(click(subtraction));
        assert_eq!(preview.preview_text(), "A: 4B: 20A + BA - B-16");
    }

    #[test]
    fn while_preview_recomputes_after_operation_selected() {
        let source = include_str!("../../../playground/frontend/src/examples/while/while.bn");
        let mut preview = WhilePreview::new(source).expect("while preview");
        assert_eq!(preview.preview_text(), "A: 0B: 0A + BA - B");

        let _ = preview.render_snapshot();
        let input_a_tick = preview
            .app()
            .event_port_for_source(preview.program.input_a_tick_port)
            .expect("input_a tick");
        let input_b_tick = preview
            .app()
            .event_port_for_source(preview.program.input_b_tick_port)
            .expect("input_b tick");
        let addition = preview
            .app()
            .event_port_for_source(preview.program.addition_press_port)
            .expect("addition");
        let subtraction = preview
            .app()
            .event_port_for_source(preview.program.subtraction_press_port)
            .expect("subtraction");

        for batch in half_then_one_second_ticks(input_a_tick, input_b_tick) {
            preview.dispatch_ui_events(batch);
        }
        preview.dispatch_ui_events(click(addition));
        assert_eq!(preview.preview_text(), "A: 2B: 10A + BA - B12");

        preview.dispatch_ui_events(one_second_tick(input_a_tick, input_b_tick));
        assert_eq!(preview.preview_text(), "A: 4B: 20A + BA - B24");

        preview.dispatch_ui_events(click(subtraction));
        assert_eq!(preview.preview_text(), "A: 4B: 20A + BA - B-16");

        preview.dispatch_ui_events(one_second_tick(input_a_tick, input_b_tick));
        assert_eq!(preview.preview_text(), "A: 6B: 30A + BA - B-24");
    }
}
