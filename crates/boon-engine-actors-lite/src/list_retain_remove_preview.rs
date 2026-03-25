use crate::bridge::{HostInput, HostSnapshot};
use crate::host_view_preview::HostViewPreviewApp;
use crate::ids::ActorId;
use crate::interactive_preview::{InteractivePreview, render_interactive_preview};
use crate::ir::SinkPortId;
use crate::ir_executor::IrExecutor;
use crate::lower::{ListRetainRemoveProgram, try_lower_list_retain_remove};
use crate::preview_runtime::PreviewRuntime;
use crate::runtime::ActorKind;
use crate::slot_projection::{project_slot_values_into_app, project_slot_values_into_map};
use boon::platform::browser::kernel::KernelValue;
use boon::zoon::*;
use boon_renderer_zoon::FakeRenderState;
use boon_scene::{RenderRoot, UiEventBatch, UiEventKind};
use std::collections::BTreeMap;

pub struct ListRetainRemovePreview {
    #[cfg_attr(not(test), allow(dead_code))]
    program: ListRetainRemoveProgram,
    runtime: PreviewRuntime,
    host_actor: ActorId,
    executor: IrExecutor,
    app: HostViewPreviewApp,
}

impl ListRetainRemovePreview {
    pub fn new(source: &str) -> Result<Self, String> {
        let program = try_lower_list_retain_remove(source)?;
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
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    fn refresh_sink_values(&mut self) {
        let current_items = self.current_items();
        self.app.set_sink_value(
            self.program.title_sink,
            self.executor
                .sink_value(self.program.title_sink)
                .cloned()
                .unwrap_or_else(|| KernelValue::from("Add items with Enter")),
        );
        self.app.set_sink_value(
            self.program.input_sink,
            self.executor
                .sink_value(self.program.input_sink)
                .cloned()
                .unwrap_or_else(|| KernelValue::from("")),
        );
        self.app.set_sink_value(
            self.program.count_sink,
            self.executor
                .sink_value(self.program.count_sink)
                .cloned()
                .unwrap_or_else(|| KernelValue::from("Count: 3")),
        );
        project_slot_values_into_app(
            &mut self.app,
            &self.program.item_sinks,
            current_items
                .into_iter()
                .map(|item| KernelValue::from(format!("- {item}"))),
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
            .expect("list_retain_remove IR should execute");
        self.refresh_sink_values();
        true
    }
}

impl InteractivePreview for ListRetainRemovePreview {
    fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool {
        let change_port = self
            .app
            .event_port_for_source(self.program.input_change_port);
        let key_port = self
            .app
            .event_port_for_source(self.program.input_key_down_port);
        let inputs = batch
            .events
            .into_iter()
            .enumerate()
            .filter_map(|(index, event)| match event.kind {
                UiEventKind::Input | UiEventKind::Change if Some(event.target) == change_port => {
                    Some(HostInput::Pulse {
                        actor: self.host_actor,
                        port: self.program.input_change_port,
                        value: KernelValue::from(event.payload.unwrap_or_default()),
                        seq: self.runtime.causal_seq(index as u32),
                    })
                }
                UiEventKind::KeyDown if Some(event.target) == key_port => Some(HostInput::Pulse {
                    actor: self.host_actor,
                    port: self.program.input_key_down_port,
                    value: KernelValue::from(event.payload.unwrap_or_default()),
                    seq: self.runtime.causal_seq(index as u32),
                }),
                _ => None,
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
    program: &ListRetainRemoveProgram,
    executor: &IrExecutor,
) -> BTreeMap<SinkPortId, KernelValue> {
    let mut sink_values = BTreeMap::new();
    sink_values.insert(
        program.title_sink,
        executor
            .sink_value(program.title_sink)
            .cloned()
            .unwrap_or_else(|| KernelValue::from("Add items with Enter")),
    );
    sink_values.insert(
        program.input_sink,
        executor
            .sink_value(program.input_sink)
            .cloned()
            .unwrap_or_else(|| KernelValue::from("")),
    );
    sink_values.insert(
        program.count_sink,
        executor
            .sink_value(program.count_sink)
            .cloned()
            .unwrap_or_else(|| KernelValue::from("Count: 3")),
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
                KernelValue::Text(text) | KernelValue::Tag(text) => {
                    Some(KernelValue::from(format!("- {text}")))
                }
                _ => None,
            }),
        KernelValue::from(""),
    );
    sink_values
}

pub fn render_list_retain_remove_preview(preview: ListRetainRemovePreview) -> impl Element {
    render_interactive_preview(preview)
}

#[cfg(test)]
mod tests {
    use super::*;
    use boon_scene::{UiEvent, UiEventKind};

    #[test]
    fn list_retain_remove_preview_appends_trimmed_items() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/list_retain_remove/list_retain_remove.bn"
        );
        let mut preview = ListRetainRemovePreview::new(source).expect("list_retain_remove preview");
        assert_eq!(
            preview.preview_text(),
            "Add items with EnterCount: 3- Apple- Banana- Cherry"
        );

        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(preview.program.input_change_port)
                    .expect("change port"),
                kind: UiEventKind::Change,
                payload: Some("  Orange  ".to_string()),
            }],
        });
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(preview.program.input_key_down_port)
                    .expect("key port"),
                kind: UiEventKind::KeyDown,
                payload: Some("Enter\u{1F}  Orange  ".to_string()),
            }],
        });

        assert_eq!(
            preview.preview_text(),
            "Add items with EnterCount: 4- Apple- Banana- Cherry- Orange"
        );
    }
}
