use crate::host_view_preview::HostViewPreviewApp;
use crate::lower::{CheckboxTestProgram, try_lower_checkbox_test};
use crate::mapped_item_state_runtime::MappedItemStateRuntime;
use boon::platform::browser::kernel::KernelValue;
use boon::zoon::*;
use boon_renderer_zoon::{
    FakeRenderState, RenderInteractionHandlers, render_snapshot_root_with_handlers,
};
use boon_scene::{RenderRoot, UiEventBatch, UiFactBatch, UiNode};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

struct CheckboxItem {
    name: &'static str,
    checked: bool,
}

pub struct CheckboxTestPreview {
    program: CheckboxTestProgram,
    app: HostViewPreviewApp,
    items: MappedItemStateRuntime<CheckboxItem, 2>,
}

impl CheckboxTestPreview {
    pub fn new(source: &str) -> Result<Self, String> {
        let program = try_lower_checkbox_test(source)?;
        let items = [
            CheckboxItem {
                name: "Item A",
                checked: false,
            },
            CheckboxItem {
                name: "Item B",
                checked: false,
            },
        ];
        let app = HostViewPreviewApp::new(
            program.host_view.clone(),
            initial_sink_values(&items, &program),
        );
        let items = MappedItemStateRuntime::new(program.checkbox_ports, items);

        Ok(Self {
            program,
            app,
            items,
        })
    }

    pub fn dispatch_ui_events(&mut self, batch: UiEventBatch) {
        let _ = self.render_root();
        if self
            .items
            .dispatch_ui_events(&self.app, batch, |_index, item| {
                item.checked = !item.checked
            })
        {
            refresh_sink_values(&mut self.app, self.items.items(), &self.program);
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
}

fn initial_sink_values(
    items: &[CheckboxItem; 2],
    program: &CheckboxTestProgram,
) -> BTreeMap<crate::ir::SinkPortId, KernelValue> {
    let mut sink_values = BTreeMap::new();
    for (label_sink, item) in program.label_sinks.iter().zip(items.iter()) {
        sink_values.insert(*label_sink, KernelValue::from(item.name));
    }
    for ((checkbox_sink, status_sink), item) in program
        .checkbox_sinks
        .iter()
        .zip(program.status_sinks.iter())
        .zip(items.iter())
    {
        sink_values.insert(*checkbox_sink, KernelValue::Bool(item.checked));
        sink_values.insert(
            *status_sink,
            KernelValue::from(if item.checked {
                "(checked)"
            } else {
                "(unchecked)"
            }),
        );
    }
    sink_values
}

fn refresh_sink_values(
    app: &mut HostViewPreviewApp,
    items: &[CheckboxItem; 2],
    program: &CheckboxTestProgram,
) {
    for (label_sink, item) in program.label_sinks.iter().zip(items.iter()) {
        app.set_sink_value(*label_sink, KernelValue::from(item.name));
    }
    for ((checkbox_sink, status_sink), item) in program
        .checkbox_sinks
        .iter()
        .zip(program.status_sinks.iter())
        .zip(items.iter())
    {
        app.set_sink_value(*checkbox_sink, KernelValue::Bool(item.checked));
        app.set_sink_value(
            *status_sink,
            KernelValue::from(if item.checked {
                "(checked)"
            } else {
                "(unchecked)"
            }),
        );
    }
}

pub fn render_checkbox_test_preview(preview: CheckboxTestPreview) -> impl Element {
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
    fn checkbox_test_preview_keeps_checkbox_state_independent() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/checkbox_test/checkbox_test.bn"
        );
        let mut preview = CheckboxTestPreview::new(source).expect("checkbox_test preview");
        assert_eq!(preview.preview_text(), "Item A(unchecked)Item B(unchecked)");

        let _ = preview.render_root();
        let first = preview
            .app
            .event_port_for_source(preview.program.checkbox_ports[0])
            .expect("first port");
        let second = preview
            .app
            .event_port_for_source(preview.program.checkbox_ports[1])
            .expect("second port");

        preview.dispatch_ui_events(UiEventBatch {
            events: vec![boon_scene::UiEvent {
                target: first,
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        assert_eq!(preview.preview_text(), "Item A(checked)Item B(unchecked)");

        preview.dispatch_ui_events(UiEventBatch {
            events: vec![boon_scene::UiEvent {
                target: second,
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        assert_eq!(preview.preview_text(), "Item A(checked)Item B(checked)");
    }
}
