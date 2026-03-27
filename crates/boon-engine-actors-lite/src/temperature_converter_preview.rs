use crate::bridge::{HostInput, HostSnapshot};
use crate::ids::ActorId;
use crate::input_form_runtime::{FormInputBinding, FormInputEvent};
use crate::ir_executor::IrExecutor;
use crate::lower::{TemperatureConverterProgram, try_lower_temperature_converter};
use crate::preview_runtime::PreviewRuntime;
use crate::runtime::ActorKind;
use crate::validated_form_runtime::ValidatedFormRuntime;
use boon::platform::browser::kernel::KernelValue;
use boon::zoon::*;
use boon_renderer_zoon::{FakeRenderState, RenderInteractionHandlers, render_retained_snapshot_signal};
use boon_scene::{RenderRoot, UiEventBatch, UiFactBatch, UiNode};
use std::cell::RefCell;
use std::rc::Rc;

pub struct TemperatureConverterPreview {
    runtime: PreviewRuntime,
    celsius_actor: ActorId,
    fahrenheit_actor: ActorId,
    program: TemperatureConverterProgram,
    executor: IrExecutor,
    form: ValidatedFormRuntime<2>,
}

impl TemperatureConverterPreview {
    pub fn new(source: &str) -> Result<Self, String> {
        let program = try_lower_temperature_converter(source)?;
        let mut runtime = PreviewRuntime::new();
        let celsius_actor = runtime.alloc_actor(ActorKind::SourcePort);
        let fahrenheit_actor = runtime.alloc_actor(ActorKind::SourcePort);
        let executor = IrExecutor::new(program.ir.clone())?;
        let bindings = [
            FormInputBinding {
                change_port: program.celsius_change_port,
                key_down_port: Some(program.celsius_key_down_port),
            },
            FormInputBinding {
                change_port: program.fahrenheit_change_port,
                key_down_port: Some(program.fahrenheit_key_down_port),
            },
        ];
        let form = ValidatedFormRuntime::new(
            program.host_view.clone(),
            executor.sink_values().clone(),
            bindings,
            [],
        );

        let mut preview = Self {
            runtime,
            celsius_actor,
            fahrenheit_actor,
            program,
            executor,
            form,
        };
        preview.sync_inputs_from_sinks();
        Ok(preview)
    }

    pub fn dispatch_ui_events(&mut self, batch: UiEventBatch) {
        let dispatch = self.form.dispatch_ui_events(batch);
        let inputs = dispatch
            .input_events
            .iter()
            .enumerate()
            .filter_map(|(index, event)| match event {
                FormInputEvent::Changed { index: 0 } => Some(HostInput::Pulse {
                    actor: self.celsius_actor,
                    port: self.program.celsius_change_port,
                    value: KernelValue::from(self.form.input(0).to_string()),
                    seq: self.runtime.causal_seq(index as u32),
                }),
                FormInputEvent::Changed { index: 1 } => Some(HostInput::Pulse {
                    actor: self.fahrenheit_actor,
                    port: self.program.fahrenheit_change_port,
                    value: KernelValue::from(self.form.input(1).to_string()),
                    seq: self.runtime.causal_seq(index as u32),
                }),
                FormInputEvent::KeyDown { .. } => None,
                FormInputEvent::Changed { .. } => None,
            })
            .collect::<Vec<_>>();

        if inputs.is_empty() {
            return;
        }

        let messages = self.runtime.dispatch_snapshot(HostSnapshot::new(inputs));
        self.executor
            .apply_messages(&messages)
            .expect("temperature converter IR should execute");
        for (sink, value) in self.executor.sink_values() {
            self.form.set_sink_value(*sink, value.clone());
        }
        self.sync_inputs_from_sinks();
    }

    #[must_use]
    pub fn preview_text(&mut self) -> String {
        self.form.preview_text()
    }

    fn render_snapshot(&mut self) -> (UiNode, FakeRenderState) {
        self.form.render_snapshot()
    }

    #[must_use]
    pub fn app(&self) -> &crate::host_view_preview::HostViewPreviewApp {
        self.form.app()
    }

    fn sync_inputs_from_sinks(&mut self) {
        let celsius = self
            .form
            .sink_value(self.program.celsius_input_sink)
            .map(render_kernel_value)
            .unwrap_or_default();
        let fahrenheit = self
            .form
            .sink_value(self.program.fahrenheit_input_sink)
            .map(render_kernel_value)
            .unwrap_or_default();
        let _ = self.form.set_input(0, celsius);
        let _ = self.form.set_input(1, fahrenheit);
    }
}

fn render_kernel_value(value: &KernelValue) -> String {
    match value {
        KernelValue::Number(number) if number.fract() == 0.0 => format!("{}", *number as i64),
        KernelValue::Number(number) => number.to_string(),
        KernelValue::Text(text) | KernelValue::Tag(text) => text.clone(),
        KernelValue::Bool(value) => value.to_string(),
        KernelValue::Skip => String::new(),
        KernelValue::Object(_) | KernelValue::List(_) => format!("{value:?}"),
    }
}

pub fn render_temperature_converter_preview(preview: TemperatureConverterPreview) -> impl Element {
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
    use boon_scene::{UiEvent, UiEventKind};

    #[test]
    fn temperature_converter_preview_updates_bidirectionally() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/temperature_converter/temperature_converter.bn"
        );
        let mut preview =
            TemperatureConverterPreview::new(source).expect("temperature_converter preview");
        assert!(preview.preview_text().contains("Temperature Converter"));

        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(preview.program.celsius_change_port)
                    .expect("celsius port"),
                kind: UiEventKind::Input,
                payload: Some("100".to_string()),
            }],
        });
        assert_eq!(
            preview
                .form
                .sink_value(preview.program.fahrenheit_input_sink),
            Some(&KernelValue::from(212.0))
        );

        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(preview.program.fahrenheit_change_port)
                    .expect("fahrenheit port"),
                kind: UiEventKind::Input,
                payload: Some("32".to_string()),
            }],
        });
        assert_eq!(
            preview.form.sink_value(preview.program.celsius_input_sink),
            Some(&KernelValue::from(0.0))
        );
    }

    #[test]
    fn clearing_fahrenheit_clears_celsius_projection() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/temperature_converter/temperature_converter.bn"
        );
        let mut preview =
            TemperatureConverterPreview::new(source).expect("temperature_converter preview");
        assert!(preview.preview_text().contains("Temperature Converter"));
        let celsius_port = preview
            .app()
            .event_port_for_source(preview.program.celsius_change_port)
            .expect("celsius port");
        let fahrenheit_port = preview
            .app()
            .event_port_for_source(preview.program.fahrenheit_change_port)
            .expect("fahrenheit port");

        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: celsius_port,
                kind: UiEventKind::Input,
                payload: Some("100".to_string()),
            }],
        });
        assert_eq!(
            preview
                .form
                .sink_value(preview.program.fahrenheit_input_sink),
            Some(&KernelValue::from(212.0))
        );

        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: fahrenheit_port,
                kind: UiEventKind::Input,
                payload: Some(String::new()),
            }],
        });
        assert_eq!(
            preview.form.sink_value(preview.program.celsius_input_sink),
            Some(&KernelValue::from(String::new()))
        );
        assert_eq!(
            preview.form.sink_value(preview.program.fahrenheit_input_sink),
            Some(&KernelValue::from(String::new()))
        );
    }
}
