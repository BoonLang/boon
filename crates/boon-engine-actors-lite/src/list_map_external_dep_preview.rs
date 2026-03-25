use crate::bridge::{HostInput, HostSnapshot};
use crate::host_view_preview::HostViewPreviewApp;
use crate::ids::ActorId;
use crate::interactive_preview::{InteractivePreview, render_interactive_preview};
use crate::ir::SinkPortId;
use crate::ir_executor::IrExecutor;
use crate::lower::{ListMapExternalDepProgram, try_lower_list_map_external_dep};
use crate::preview_runtime::PreviewRuntime;
use crate::runtime::ActorKind;
use crate::slot_projection::{project_slot_values_into_app, project_slot_values_into_map};
use boon::platform::browser::kernel::KernelValue;
use boon::zoon::*;
use boon_renderer_zoon::FakeRenderState;
use boon_scene::{RenderRoot, UiEventBatch, UiEventKind};
use std::collections::BTreeMap;

pub struct ListMapExternalDepPreview {
    #[cfg_attr(not(test), allow(dead_code))]
    program: ListMapExternalDepProgram,
    runtime: PreviewRuntime,
    host_actor: ActorId,
    executor: IrExecutor,
    app: HostViewPreviewApp,
}

impl ListMapExternalDepPreview {
    pub fn new(source: &str) -> Result<Self, String> {
        let program = try_lower_list_map_external_dep(source)?;
        let mut runtime = PreviewRuntime::new();
        let host_actor = runtime.alloc_actor(ActorKind::SourcePort);
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
    pub fn app(&self) -> &HostViewPreviewApp {
        &self.app
    }

    #[must_use]
    pub fn preview_text(&mut self) -> String {
        self.app.preview_text()
    }

    fn current_items(&self) -> Vec<String> {
        match self.executor.sink_value(self.program.items_list_sink) {
            Some(KernelValue::List(items)) => items
                .iter()
                .filter_map(|item| match item {
                    KernelValue::Text(text) | KernelValue::Tag(text) => Some(text.clone()),
                    KernelValue::Skip => None,
                    _ => None,
                })
                .collect(),
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
                .unwrap_or_else(|| KernelValue::from("show_filtered: False")),
        );
        self.app.set_sink_value(
            self.program.info_sink,
            self.executor
                .sink_value(self.program.info_sink)
                .cloned()
                .unwrap_or_else(|| {
                    KernelValue::from(
                        "Expected: When True, show Apple and Cherry. When False, show all.",
                    )
                }),
        );
        project_slot_values_into_app(
            &mut self.app,
            &self.program.item_sinks,
            items.into_iter().map(KernelValue::from),
            KernelValue::from(""),
        );
    }

    fn apply_messages(&mut self, inputs: Vec<HostInput>) -> bool {
        if inputs.is_empty() {
            return false;
        }
        let messages = self.runtime.dispatch_snapshot(HostSnapshot::new(inputs));
        self.executor
            .apply_messages(&messages)
            .expect("list_map_external_dep IR should execute");
        self.refresh_sink_values();
        true
    }
}

impl InteractivePreview for ListMapExternalDepPreview {
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

    fn dispatch_ui_facts(&mut self, _batch: boon_scene::UiFactBatch) -> bool {
        false
    }

    fn render_snapshot(&mut self) -> (RenderRoot, FakeRenderState) {
        let (root, state) = self.app.render_snapshot();
        (RenderRoot::UiTree(root), state)
    }
}

fn initial_sink_values(
    program: &ListMapExternalDepProgram,
    executor: &IrExecutor,
) -> BTreeMap<SinkPortId, KernelValue> {
    let mut sink_values = BTreeMap::new();
    sink_values.insert(
        program.mode_sink,
        executor
            .sink_value(program.mode_sink)
            .cloned()
            .unwrap_or_else(|| KernelValue::from("show_filtered: False")),
    );
    sink_values.insert(
        program.info_sink,
        executor
            .sink_value(program.info_sink)
            .cloned()
            .unwrap_or_else(|| {
                KernelValue::from(
                    "Expected: When True, show Apple and Cherry. When False, show all.",
                )
            }),
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
            .unwrap_or_default()
            .into_iter()
            .filter_map(|item| match item {
                KernelValue::Text(text) | KernelValue::Tag(text) => Some(KernelValue::from(text)),
                KernelValue::Skip => None,
                _ => None,
            }),
        KernelValue::from(""),
    );
    sink_values
}

pub fn render_list_map_external_dep_preview(preview: ListMapExternalDepPreview) -> impl Element {
    render_interactive_preview(preview)
}

#[cfg(test)]
mod tests {
    use super::*;
    use boon_scene::{UiEvent, UiEventKind};

    #[test]
    fn list_map_external_dep_preview_updates_visible_items() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/list_map_external_dep/list_map_external_dep.bn"
        );
        let mut preview =
            ListMapExternalDepPreview::new(source).expect("list_map_external_dep preview");
        assert_eq!(
            preview.preview_text(),
            "show_filtered: FalseToggle filterExpected: When True, show Apple and Cherry. When False, show all.AppleBananaCherryDate"
        );

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
            "show_filtered: TrueToggle filterExpected: When True, show Apple and Cherry. When False, show all.AppleCherry"
        );

        preview.click_toggle();
        assert_eq!(
            preview.preview_text(),
            "show_filtered: FalseToggle filterExpected: When True, show Apple and Cherry. When False, show all.AppleBananaCherryDate"
        );
    }
}
