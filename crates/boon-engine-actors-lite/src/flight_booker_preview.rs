use crate::bridge::HostInput;
use crate::ids::ActorId;
use crate::input_form_runtime::{FormInputBinding, FormInputEvent};
use crate::ir_executor::IrExecutor;
use crate::lower::{FlightBookerProgram, try_lower_flight_booker};
use crate::preview_runtime::PreviewRuntime;
use crate::validated_form_runtime::ValidatedFormRuntime;
use boon::platform::browser::kernel::KernelValue;
use boon::zoon::*;
use boon_renderer_zoon::{
    FakeRenderState, RenderInteractionHandlers, render_snapshot_root_with_handlers,
};
use boon_scene::{RenderRoot, UiEventBatch, UiFactBatch, UiNode};
use std::cell::RefCell;
use std::rc::Rc;

pub struct FlightBookerPreview {
    runtime: PreviewRuntime,
    flight_type_actor: ActorId,
    departure_actor: ActorId,
    return_actor: ActorId,
    book_actor: ActorId,
    program: FlightBookerProgram,
    executor: IrExecutor,
    form: ValidatedFormRuntime<3>,
}

impl FlightBookerPreview {
    pub fn new(source: &str) -> Result<Self, String> {
        let program = try_lower_flight_booker(source)?;
        let mut runtime = PreviewRuntime::new();
        let flight_type_actor = runtime.alloc_actor();
        let departure_actor = runtime.alloc_actor();
        let return_actor = runtime.alloc_actor();
        let book_actor = runtime.alloc_actor();
        let executor = IrExecutor::new(program.ir.clone())?;
        let form = ValidatedFormRuntime::new(
            program.host_view.clone(),
            executor.sink_values(),
            [
                FormInputBinding {
                    change_port: program.flight_type_change_port,
                    key_down_port: None,
                },
                FormInputBinding {
                    change_port: program.departure_change_port,
                    key_down_port: None,
                },
                FormInputBinding {
                    change_port: program.return_change_port,
                    key_down_port: None,
                },
            ],
            [program.book_press_port],
        );

        Ok(Self {
            runtime,
            flight_type_actor,
            departure_actor,
            return_actor,
            book_actor,
            program,
            executor,
            form,
        })
    }

    pub fn dispatch_ui_events(&mut self, batch: UiEventBatch) {
        let dispatch = self.form.dispatch_ui_events(batch);
        let mut inputs = dispatch
            .input_events
            .iter()
            .enumerate()
            .filter_map(|(seq, event)| match event {
                FormInputEvent::Changed { index: 0 } => Some(HostInput::Pulse {
                    actor: self.flight_type_actor,
                    port: self.program.flight_type_change_port,
                    value: KernelValue::from(self.form.input(0).to_string()),
                    seq: self.runtime.causal_seq(seq as u32),
                }),
                FormInputEvent::Changed { index: 1 } => Some(HostInput::Pulse {
                    actor: self.departure_actor,
                    port: self.program.departure_change_port,
                    value: KernelValue::from(self.form.input(1).to_string()),
                    seq: self.runtime.causal_seq(seq as u32),
                }),
                FormInputEvent::Changed { index: 2 } => Some(HostInput::Pulse {
                    actor: self.return_actor,
                    port: self.program.return_change_port,
                    value: KernelValue::from(self.form.input(2).to_string()),
                    seq: self.runtime.causal_seq(seq as u32),
                }),
                FormInputEvent::KeyDown { .. } | FormInputEvent::Changed { .. } => None,
            })
            .collect::<Vec<_>>();

        let input_seq_base = inputs.len() as u32;
        inputs.extend(dispatch.clicked_ports.into_iter().enumerate().filter_map(
            |(offset, port)| {
                (port == self.program.book_press_port).then_some(HostInput::Pulse {
                    actor: self.book_actor,
                    port,
                    value: KernelValue::from("press"),
                    seq: self.runtime.causal_seq(input_seq_base + offset as u32),
                })
            },
        ));

        if inputs.is_empty() {
            return;
        }

        let (runtime, executor, form) = (&mut self.runtime, &mut self.executor, &mut self.form);
        runtime.dispatch_inputs_batches(inputs.as_slice(), |messages| {
            executor
                .apply_pure_messages_owned(messages.drain(..))
                .expect("flight booker IR should execute");
        });
        for (sink, value) in executor.sink_values() {
            form.set_sink_value(sink, value);
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
    #[cfg(test)]
    pub(crate) fn app(&self) -> &crate::host_view_preview::HostViewPreviewApp {
        self.form.app()
    }
}

pub fn render_flight_booker_preview(preview: FlightBookerPreview) -> impl Element {
    let preview = Rc::new(RefCell::new(preview));
    let version = Mutable::new(0u64);

    let handlers = RenderInteractionHandlers::new(
        {
            let preview = preview.clone();
            let version = version.clone();
            move |batch: UiEventBatch| {
                preview.borrow_mut().dispatch_ui_events(batch);
                version.update(|value| value + 1);
            }
        },
        move |_facts: UiFactBatch| {},
    );

    El::new().child_signal(version.signal().map({
        let preview = preview.clone();
        let handlers = handlers.clone();
        move |_| {
            let (root, state) = preview.borrow_mut().render_snapshot();
            let root = RenderRoot::UiTree(root);
            Some(render_snapshot_root_with_handlers(&root, &state, &handlers))
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use boon_scene::UiEventKind;

    #[test]
    fn flight_booker_preview_switches_modes_and_books() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/flight_booker/flight_booker.bn"
        );
        let mut preview = FlightBookerPreview::new(source).expect("flight_booker preview");
        assert!(preview.preview_text().contains("Flight Booker"));
        assert!(preview.preview_text().contains("One-way flight"));

        let _ = preview.render_root();
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![boon_scene::UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(preview.program.book_press_port)
                    .expect("book port"),
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        assert!(
            preview
                .preview_text()
                .contains("Booked one-way flight on 2026-03-03")
        );

        preview.dispatch_ui_events(UiEventBatch {
            events: vec![boon_scene::UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(preview.program.flight_type_change_port)
                    .expect("select port"),
                kind: UiEventKind::Input,
                payload: Some("return".to_string()),
            }],
        });
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![boon_scene::UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(preview.program.book_press_port)
                    .expect("book port"),
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        assert!(
            preview
                .preview_text()
                .contains("Booked return flight: 2026-03-03 to 2026-03-03")
        );
    }
}
