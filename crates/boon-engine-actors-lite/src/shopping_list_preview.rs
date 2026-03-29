use crate::bridge::HostInput;
use crate::host_view_preview::HostViewPreviewApp;
use crate::ids::ActorId;
use crate::interactive_preview::{InteractivePreview, render_interactive_preview};
use crate::ir::SinkPortId;
use crate::ir_executor::IrExecutor;
use crate::lower::{ShoppingListProgram, try_lower_shopping_list};
use crate::preview_runtime::PreviewRuntime;
use crate::slot_projection::{project_slot_values_into_app, project_slot_values_into_map};
use boon::platform::browser::kernel::KernelValue;
use boon::zoon::*;
use boon_renderer_zoon::FakeRenderState;
use boon_scene::{RenderRoot, UiEventBatch, UiEventKind};
use std::collections::BTreeMap;

pub struct ShoppingListPreview {
    #[cfg_attr(not(test), allow(dead_code))]
    program: ShoppingListProgram,
    runtime: PreviewRuntime,
    host_actor: ActorId,
    executor: IrExecutor,
    app: HostViewPreviewApp,
}

impl ShoppingListPreview {
    pub fn new(source: &str) -> Result<Self, String> {
        let program = try_lower_shopping_list(source)?;
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

    #[must_use]
    #[cfg(test)]
    pub(crate) fn app(&self) -> &HostViewPreviewApp {
        &self.app
    }

    #[must_use]
    pub fn preview_text(&mut self) -> String {
        self.app.preview_text()
    }

    fn refresh_sink_values(&mut self) {
        self.app.set_sink_value(
            self.program.title_sink,
            self.executor
                .sink_value(self.program.title_sink)
                .cloned()
                .unwrap_or_else(|| KernelValue::from("Shopping List")),
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
                .unwrap_or_else(|| KernelValue::from("0 items")),
        );
        let current_items = self.current_items();
        project_slot_values_into_app(
            &mut self.app,
            &self.program.item_sinks,
            current_items
                .into_iter()
                .map(|item| KernelValue::from(format!("- {item}"))),
            KernelValue::from(""),
        );
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
                .expect("shopping_list IR should execute");
        });
        self.refresh_sink_values();
        true
    }
}

impl InteractivePreview for ShoppingListPreview {
    fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool {
        let change_port = self
            .app
            .event_port_for_source(self.program.input_change_port);
        let key_port = self
            .app
            .event_port_for_source(self.program.input_key_down_port);
        let clear_port = self
            .app
            .event_port_for_source(self.program.clear_press_port);
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
                UiEventKind::Click if Some(event.target) == clear_port => Some(HostInput::Pulse {
                    actor: self.host_actor,
                    port: self.program.clear_press_port,
                    value: KernelValue::from("press"),
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
    program: &ShoppingListProgram,
    executor: &IrExecutor,
) -> BTreeMap<SinkPortId, KernelValue> {
    let mut sink_values = BTreeMap::new();
    sink_values.insert(
        program.title_sink,
        executor
            .sink_value(program.title_sink)
            .cloned()
            .unwrap_or_else(|| KernelValue::from("Shopping List")),
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
            .unwrap_or_else(|| KernelValue::from("0 items")),
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

pub fn render_shopping_list_preview(preview: ShoppingListPreview) -> impl Element {
    render_interactive_preview(preview)
}

#[cfg(test)]
mod tests {
    use super::*;
    use boon_scene::{UiEvent, UiEventKind};

    #[test]
    fn shopping_list_preview_adds_and_clears_items() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/shopping_list/shopping_list.bn"
        );
        let mut preview = ShoppingListPreview::new(source).expect("shopping_list preview");
        assert!(preview.preview_text().contains("0 items"));

        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(preview.program.input_change_port)
                    .expect("change port"),
                kind: UiEventKind::Change,
                payload: Some("Milk".to_string()),
            }],
        });
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(preview.program.input_key_down_port)
                    .expect("key port"),
                kind: UiEventKind::KeyDown,
                payload: Some("Enter\u{1F}Milk".to_string()),
            }],
        });
        assert!(preview.preview_text().contains("1 items"));
        assert!(preview.preview_text().contains("- Milk"));

        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(preview.program.clear_press_port)
                    .expect("clear port"),
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        assert!(preview.preview_text().contains("0 items"));
        assert!(!preview.preview_text().contains("- Milk"));
    }
}
