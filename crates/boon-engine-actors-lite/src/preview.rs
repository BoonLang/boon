use crate::bridge::HostInput;
use crate::clock::MonotonicInstant;
use crate::host_view_preview::HostViewPreviewApp;
use crate::ids::ActorId;
use crate::ir_executor::IrExecutor;
use crate::lower::{CounterProgram, lower_program};
use crate::metrics::{CounterMetricsReport, LatencySummary};
use crate::preview_runtime::PreviewRuntime;
use crate::runtime::RuntimeTelemetrySnapshot;
use boon::platform::browser::kernel::KernelValue;
use boon::zoon::*;
use boon_renderer_zoon::{
    FakeRenderState, RenderInteractionHandlers, render_retained_snapshot_signal,
};
use boon_scene::{RenderRoot, UiEventBatch, UiEventKind, UiFactBatch, UiNode};
use std::cell::RefCell;
use std::rc::Rc;
pub struct CounterPreview {
    runtime: PreviewRuntime,
    button_actor: ActorId,
    program: CounterProgram,
    executor: IrExecutor,
    app: HostViewPreviewApp,
}

impl CounterPreview {
    pub fn new(source: &str) -> Result<Self, String> {
        Self::from_program(lower_program(source)?.into_counter_program()?)
    }

    pub fn from_program(program: CounterProgram) -> Result<Self, String> {
        let mut runtime = PreviewRuntime::new();
        let button_actor = runtime.alloc_actor();
        let executor = IrExecutor::new(program.ir.clone())?;

        let sink_values = executor.sink_values();
        let app = HostViewPreviewApp::new(program.host_view.clone(), sink_values);

        Ok(Self {
            runtime,
            button_actor,
            program,
            executor,
            app,
        })
    }

    pub fn click_increment(&mut self) {
        let Self {
            runtime,
            button_actor,
            program,
            executor,
            app,
        } = self;
        runtime.dispatch_pulse_batches(
            *button_actor,
            program.press_port,
            KernelValue::from("press"),
            |messages| Self::apply_runtime_messages(executor, app, messages),
        );
    }

    pub fn dispatch_inputs(&mut self, inputs: &[HostInput]) {
        let Self {
            runtime,
            executor,
            app,
            ..
        } = self;
        runtime.dispatch_inputs_batches(inputs, |messages| {
            Self::apply_runtime_messages(executor, app, messages)
        });
    }

    pub fn dispatch_ui_events(&mut self, batch: UiEventBatch) {
        let root = self.render_root();
        let button_port = root
            .children
            .first()
            .and_then(|stripe| stripe.children.get(1))
            .and_then(|button| match &button.kind {
                boon_scene::UiNodeKind::Element { event_ports, .. } => event_ports.first().copied(),
                _ => None,
            });
        for event in batch.events {
            if Some(event.target) == button_port && event.kind == UiEventKind::Click {
                self.click_increment();
            }
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

    fn apply_runtime_messages(
        executor: &mut IrExecutor,
        app: &mut HostViewPreviewApp,
        messages: &mut Vec<crate::runtime::Msg>,
    ) {
        executor
            .apply_pure_messages_owned(messages.drain(..))
            .expect("counter IR should execute");
        for (sink, value) in executor.sink_values() {
            app.set_sink_value(sink, value);
        }
    }

    #[must_use]
    pub(crate) fn runtime_telemetry_snapshot(&self) -> RuntimeTelemetrySnapshot {
        self.runtime.telemetry_snapshot()
    }
}

pub(crate) fn counter_metrics_capture()
-> Result<(CounterMetricsReport, RuntimeTelemetrySnapshot), String> {
    let source = include_str!("../../../playground/frontend/src/examples/counter/counter.bn");

    let startup_started = MonotonicInstant::now();
    let mut preview = CounterPreview::new(source)?;
    let _ = preview.preview_text();
    let startup_millis = startup_started.elapsed().as_secs_f64() * 1000.0;

    let mut press_samples = Vec::new();
    for _ in 0..64 {
        let started = MonotonicInstant::now();
        preview.click_increment();
        let _ = preview.preview_text();
        press_samples.push(started.elapsed());
    }

    Ok((
        CounterMetricsReport {
            startup_millis,
            press_to_paint: LatencySummary::from_durations(&press_samples),
        },
        preview.runtime_telemetry_snapshot(),
    ))
}

pub fn counter_metrics_snapshot() -> Result<CounterMetricsReport, String> {
    counter_metrics_capture().map(|(report, _telemetry)| report)
}

pub fn render_counter_preview(preview: CounterPreview) -> impl Element {
    let preview = Rc::new(RefCell::new(preview));
    let snapshot = Mutable::new({
        let (root, state) = preview.borrow_mut().render_snapshot();
        (RenderRoot::UiTree(root), state)
    });

    let handlers = RenderInteractionHandlers::new(
        {
            let preview = preview.clone();
            let snapshot = snapshot.clone();
            move |batch: UiEventBatch| {
                preview.borrow_mut().dispatch_ui_events(batch);
                let (root, state) = preview.borrow_mut().render_snapshot();
                snapshot.set((RenderRoot::UiTree(root), state));
            }
        },
        move |_facts: UiFactBatch| {},
    );

    render_retained_snapshot_signal(snapshot.signal_cloned(), handlers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::HostInput;
    use crate::counter_acceptance::{CounterAcceptanceAction, counter_acceptance_sequences};
    use crate::semantics::CausalSeq;

    #[derive(Clone)]
    struct TraceRng(u64);

    impl TraceRng {
        fn new(seed: u64) -> Self {
            Self(seed)
        }

        fn next_u32(&mut self) -> u32 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1);
            (self.0 >> 32) as u32
        }

        fn next_range(&mut self, upper: u32) -> u32 {
            self.next_u32() % upper
        }
    }

    #[test]
    fn counter_preview_renders_initial_and_incremented_text() {
        let source = include_str!("../../../playground/frontend/src/examples/counter/counter.bn");
        let mut preview = CounterPreview::new(source).expect("counter preview");
        assert_eq!(preview.preview_text(), "0+");
        preview.click_increment();
        preview.click_increment();
        preview.click_increment();
        assert_eq!(preview.preview_text(), "3+");
    }

    #[test]
    fn counter_preview_preserves_retained_ids_across_updates() {
        let source = include_str!("../../../playground/frontend/src/examples/counter/counter.bn");
        let mut preview = CounterPreview::new(source).expect("counter preview");
        let initial = preview.render_root();
        let initial_label = initial.children[0].children[0].id;
        let initial_button = initial.children[0].children[1].id;
        preview.click_increment();
        let rerendered = preview.render_root();
        assert_eq!(rerendered.children[0].children[0].id, initial_label);
        assert_eq!(rerendered.children[0].children[1].id, initial_button);
    }

    #[test]
    fn counter_preview_batches_multiple_pulses_from_one_snapshot() {
        let source = include_str!("../../../playground/frontend/src/examples/counter/counter.bn");
        let mut preview = CounterPreview::new(source).expect("counter preview");
        let actor = preview.button_actor;
        let port = preview.program.press_port;
        preview.dispatch_inputs(&[
            HostInput::Pulse {
                actor,
                port,
                value: KernelValue::from("press"),
                seq: CausalSeq::new(1, 0),
            },
            HostInput::Pulse {
                actor,
                port,
                value: KernelValue::from("press"),
                seq: CausalSeq::new(1, 1),
            },
            HostInput::Pulse {
                actor,
                port,
                value: KernelValue::from("press"),
                seq: CausalSeq::new(1, 2),
            },
        ]);
        assert_eq!(preview.preview_text(), "3+");
    }

    #[test]
    fn counter_preview_randomized_trace_matches_oracle_count() {
        let source = include_str!("../../../playground/frontend/src/examples/counter/counter.bn");
        let mut preview = CounterPreview::new(source).expect("counter preview");
        let actor = preview.button_actor;
        let port = preview.program.press_port;
        let mut expected = 0i64;
        let mut rng = TraceRng::new(0xC0FFEE);

        for turn in 1..=64u64 {
            let batch_len = 1 + rng.next_range(4);
            let mut inputs = Vec::new();
            for index in 0..batch_len {
                inputs.push(HostInput::Pulse {
                    actor,
                    port,
                    value: KernelValue::from("press"),
                    seq: CausalSeq::new(turn, index),
                });
            }
            expected += batch_len as i64;
            preview.dispatch_inputs(inputs.as_slice());
            assert_eq!(preview.preview_text(), format!("{expected}+"));
        }
    }

    #[test]
    fn counter_preview_shared_acceptance_sequences_behave_as_expected() {
        let source = include_str!("../../../playground/frontend/src/examples/counter/counter.bn");
        let mut preview = CounterPreview::new(source).expect("counter preview");

        for sequence in counter_acceptance_sequences() {
            for action in &sequence.actions {
                match action {
                    CounterAcceptanceAction::ClickButton { index } => {
                        assert_eq!(*index, 0, "counter acceptance uses increment button");
                        preview.click_increment();
                    }
                }
            }

            assert_eq!(
                preview.preview_text(),
                sequence.expect,
                "{}",
                sequence.description
            );
        }
    }
}
