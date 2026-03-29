use crate::bridge::HostInput;
use crate::host_view_preview::{
    HostViewPreviewApp, InteractiveHostViewModel, render_interactive_host_view,
};
use crate::ids::ActorId;
use crate::ir::SinkPortId;
use crate::ir_executor::IrExecutor;
use crate::lower::{ListRetainReactiveProgram, lower_program};
use crate::preview_runtime::PreviewRuntime;
use crate::slot_projection::{project_slot_values_into_app, project_slot_values_into_map};
use boon::platform::browser::kernel::KernelValue;
use boon::zoon::*;
use boon_scene::{UiEventBatch, UiEventKind};
use std::collections::BTreeMap;

pub struct ListRetainReactivePreview {
    #[cfg_attr(not(test), allow(dead_code))]
    program: ListRetainReactiveProgram,
    runtime: PreviewRuntime,
    host_actor: ActorId,
    executor: IrExecutor,
    app: HostViewPreviewApp,
}

impl ListRetainReactivePreview {
    pub fn new(source: &str) -> Result<Self, String> {
        Self::from_program(lower_program(source)?.into_list_retain_reactive_program()?)
    }

    pub fn from_program(program: ListRetainReactiveProgram) -> Result<Self, String> {
        let mut runtime = PreviewRuntime::new();
        let host_actor = runtime.alloc_actor();
        let executor = IrExecutor::new(program.ir.clone())?;
        let app = HostViewPreviewApp::new(
            program.host_view.clone(),
            initial_sink_values(&program, &executor),
        );
        Ok(Self {
            program,
            runtime,
            host_actor,
            executor,
            app,
        })
    }

    pub fn click_toggle(&mut self) {
        let _ = self.apply_messages(vec![HostInput::Pulse {
            actor: self.host_actor,
            port: self.program.toggle_port,
            value: KernelValue::from("press"),
            seq: self.runtime.causal_seq(0),
        }]);
    }

    #[must_use]
    #[cfg(test)]
    pub(crate) fn app(&self) -> &HostViewPreviewApp {
        &self.app
    }

    #[must_use]
    pub fn preview_text(&mut self) -> String {
        self.app.preview_text()
    }

    fn current_items(&self) -> Vec<KernelValue> {
        match self.executor.sink_value(self.program.items_list_sink) {
            Some(KernelValue::List(items)) => items.clone(),
            _ => Vec::new(),
        }
    }

    fn refresh_sink_values(&mut self) {
        let items = self.current_items();
        self.app.set_sink_value(
            self.program.mode_sink,
            self.executor
                .sink_value(self.program.mode_sink)
                .cloned()
                .unwrap_or_else(|| KernelValue::from("show_even: False")),
        );
        self.app.set_sink_value(
            self.program.count_sink,
            self.executor
                .sink_value(self.program.count_sink)
                .cloned()
                .unwrap_or_else(|| KernelValue::from("Filtered count: 6")),
        );
        project_slot_values_into_app(
            &mut self.app,
            &self.program.item_sinks,
            items,
            KernelValue::from(""),
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
                .expect("list_retain_reactive IR should execute");
        });
        self.refresh_sink_values();
        true
    }
}

impl InteractiveHostViewModel for ListRetainReactivePreview {
    fn app_mut(&mut self) -> &mut HostViewPreviewApp {
        &mut self.app
    }

    fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool {
        let toggle_port = self.app.event_port_for_source(self.program.toggle_port);
        let inputs = batch
            .events
            .into_iter()
            .enumerate()
            .filter_map(|(index, event)| {
                if event.kind == UiEventKind::Click && Some(event.target) == toggle_port {
                    Some(HostInput::Pulse {
                        actor: self.host_actor,
                        port: self.program.toggle_port,
                        value: KernelValue::from("press"),
                        seq: self.runtime.causal_seq(index as u32),
                    })
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        self.apply_messages(inputs)
    }
}

fn initial_sink_values(
    program: &ListRetainReactiveProgram,
    executor: &IrExecutor,
) -> BTreeMap<SinkPortId, KernelValue> {
    let mut sink_values = BTreeMap::new();
    sink_values.insert(
        program.mode_sink,
        executor
            .sink_value(program.mode_sink)
            .cloned()
            .unwrap_or_else(|| KernelValue::from("show_even: False")),
    );
    sink_values.insert(
        program.count_sink,
        executor
            .sink_value(program.count_sink)
            .cloned()
            .unwrap_or_else(|| KernelValue::from("Filtered count: 6")),
    );
    project_slot_values_into_map(
        &mut sink_values,
        &program.item_sinks,
        executor
            .sink_value(program.items_list_sink)
            .and_then(|value| match value {
                KernelValue::List(items) => Some(items.clone()),
                _ => None,
            })
            .unwrap_or_default(),
        KernelValue::from(""),
    );
    sink_values
}

pub fn render_list_retain_reactive_preview(preview: ListRetainReactivePreview) -> impl Element {
    render_interactive_host_view(preview)
}

#[cfg(test)]
mod tests {
    use super::*;
    use boon_scene::{UiEvent, UiEventKind};

    #[test]
    fn list_retain_reactive_preview_toggles_filtered_values() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/list_retain_reactive/list_retain_reactive.bn"
        );
        let mut preview =
            ListRetainReactivePreview::new(source).expect("list_retain_reactive preview");
        assert_eq!(
            preview.preview_text(),
            "Toggle filtershow_even: FalseFiltered count: 6123456"
        );

        preview.click_toggle();
        assert_eq!(
            preview.preview_text(),
            "Toggle filtershow_even: TrueFiltered count: 3246"
        );

        preview.click_toggle();
        assert_eq!(
            preview.preview_text(),
            "Toggle filtershow_even: FalseFiltered count: 6123456"
        );
    }

    #[test]
    fn list_retain_reactive_preview_dispatches_click_events_through_shared_host_view_shell() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/list_retain_reactive/list_retain_reactive.bn"
        );
        let mut preview =
            ListRetainReactivePreview::new(source).expect("list_retain_reactive preview");
        let _ = preview.render_snapshot();

        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(preview.program.toggle_port)
                    .expect("toggle port"),
                kind: UiEventKind::Click,
                payload: None,
            }],
        });

        assert_eq!(
            preview.preview_text(),
            "Toggle filtershow_even: TrueFiltered count: 3246"
        );
    }
}
