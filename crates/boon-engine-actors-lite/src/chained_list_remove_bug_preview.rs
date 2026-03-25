use crate::host_view_preview::HostViewPreviewApp;
use crate::lower::{ChainedListRemoveBugProgram, try_lower_chained_list_remove_bug};
use crate::mapped_click_runtime::MappedClickRuntime;
use crate::mapped_list_view_runtime::MappedListViewRuntime;
use boon::platform::browser::kernel::KernelValue;
use boon::zoon::*;
use boon_renderer_zoon::{
    FakeRenderState, RenderInteractionHandlers, render_snapshot_root_with_handlers,
};
use boon_scene::{RenderRoot, UiEventBatch, UiFactBatch, UiNode};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

struct ChainedListItem {
    name: String,
    completed: bool,
}

pub struct ChainedListRemoveBugPreview {
    program: ChainedListRemoveBugProgram,
    app: HostViewPreviewApp,
    clicks: MappedClickRuntime,
    items: MappedListViewRuntime<ChainedListItem>,
}

impl ChainedListRemoveBugPreview {
    pub fn new(source: &str) -> Result<Self, String> {
        let program = try_lower_chained_list_remove_bug(source)?;
        let items = MappedListViewRuntime::new(
            [
                (
                    0,
                    ChainedListItem {
                        name: "Item A".to_string(),
                        completed: false,
                    },
                ),
                (
                    1,
                    ChainedListItem {
                        name: "Item B".to_string(),
                        completed: false,
                    },
                ),
            ],
            2,
        );
        let app = HostViewPreviewApp::new(
            program.host_view.clone(),
            initial_sink_values(&program, &items),
        );
        let clicks = MappedClickRuntime::new(
            [program.add_press_port, program.clear_completed_port]
                .into_iter()
                .chain(program.checkbox_ports)
                .chain(program.remove_ports),
        );

        Ok(Self {
            program,
            app,
            clicks,
            items,
        })
    }

    pub fn dispatch_ui_events(&mut self, batch: UiEventBatch) {
        let _ = self.render_root();
        let clicked = self.clicks.dispatch_clicks(&self.app, batch);
        if !clicked.is_empty() && self.apply_clicked_ports(clicked) {
            refresh_sink_values(&mut self.app, &self.program, &self.items);
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

    fn apply_clicked_ports(&mut self, clicked: Vec<crate::ir::SourcePortId>) -> bool {
        let mut changed = false;

        for port in clicked {
            if port == self.program.add_press_port {
                self.items.append(ChainedListItem {
                    name: "New Item".to_string(),
                    completed: false,
                });
                changed = true;
                continue;
            }
            if port == self.program.clear_completed_port {
                changed |= self.items.retain(|item| !item.value.completed);
                continue;
            }
            if let Some(index) = self
                .program
                .checkbox_ports
                .iter()
                .position(|candidate| *candidate == port)
            {
                changed |= self.items.update_visible(
                    index,
                    |_| true,
                    |item| {
                        item.value.completed = !item.value.completed;
                    },
                );
                continue;
            }
            if let Some(index) = self
                .program
                .remove_ports
                .iter()
                .position(|candidate| *candidate == port)
            {
                changed |= self.items.remove_visible(index, |_| true);
            }
        }

        changed
    }
}

fn initial_sink_values(
    program: &ChainedListRemoveBugProgram,
    items: &MappedListViewRuntime<ChainedListItem>,
) -> BTreeMap<crate::ir::SinkPortId, KernelValue> {
    let mut sink_values = BTreeMap::new();
    sink_values.insert(
        program.title_sink,
        KernelValue::from("Chained List/remove Bug Test"),
    );
    refresh_sink_values_into(&mut sink_values, program, items);
    sink_values
}

fn refresh_sink_values(
    app: &mut HostViewPreviewApp,
    program: &ChainedListRemoveBugProgram,
    items: &MappedListViewRuntime<ChainedListItem>,
) {
    app.set_sink_value(
        program.title_sink,
        KernelValue::from("Chained List/remove Bug Test"),
    );
    let active_count = items
        .items()
        .iter()
        .filter(|item| !item.value.completed)
        .count();
    let completed_count = items
        .items()
        .iter()
        .filter(|item| item.value.completed)
        .count();
    app.set_sink_value(
        program.counts_sink,
        KernelValue::from(format!(
            "Active: {active_count}, Completed: {completed_count}"
        )),
    );
    items.project_visible_into_app(
        app,
        &program.checkbox_sinks,
        |_| true,
        |item| KernelValue::Bool(item.value.completed),
        KernelValue::Bool(false),
    );
    items.project_visible_into_app(
        app,
        &program.row_label_sinks,
        |_| true,
        |item| KernelValue::from(format!("{} (id={})", item.value.name, item.id)),
        KernelValue::from(""),
    );
}

fn refresh_sink_values_into(
    sink_values: &mut BTreeMap<crate::ir::SinkPortId, KernelValue>,
    program: &ChainedListRemoveBugProgram,
    items: &MappedListViewRuntime<ChainedListItem>,
) {
    let active_count = items
        .items()
        .iter()
        .filter(|item| !item.value.completed)
        .count();
    let completed_count = items
        .items()
        .iter()
        .filter(|item| item.value.completed)
        .count();
    sink_values.insert(
        program.counts_sink,
        KernelValue::from(format!(
            "Active: {active_count}, Completed: {completed_count}"
        )),
    );
    items.project_visible_into_map(
        sink_values,
        &program.checkbox_sinks,
        |_| true,
        |item| KernelValue::Bool(item.value.completed),
        KernelValue::Bool(false),
    );
    items.project_visible_into_map(
        sink_values,
        &program.row_label_sinks,
        |_| true,
        |item| KernelValue::from(format!("{} (id={})", item.value.name, item.id)),
        KernelValue::from(""),
    );
}

pub fn render_chained_list_remove_bug_preview(
    preview: ChainedListRemoveBugPreview,
) -> impl Element {
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
    fn chained_list_remove_bug_preview_keeps_cleared_items_removed() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/chained_list_remove_bug/chained_list_remove_bug.bn"
        );
        let mut preview =
            ChainedListRemoveBugPreview::new(source).expect("chained_list_remove_bug preview");
        assert!(preview.preview_text().contains("Item A (id=0)"));
        assert!(preview.preview_text().contains("Item B (id=1)"));

        let _ = preview.render_root();
        let checkbox_0 = preview
            .app
            .event_port_for_source(preview.program.checkbox_ports[0])
            .unwrap();
        let clear = preview
            .app
            .event_port_for_source(preview.program.clear_completed_port)
            .unwrap();
        let add = preview
            .app
            .event_port_for_source(preview.program.add_press_port)
            .unwrap();
        let remove_slot_1 = preview
            .app
            .event_port_for_source(preview.program.remove_ports[1])
            .unwrap();

        preview.dispatch_ui_events(UiEventBatch {
            events: vec![boon_scene::UiEvent {
                target: checkbox_0,
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![boon_scene::UiEvent {
                target: clear,
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![boon_scene::UiEvent {
                target: add,
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![boon_scene::UiEvent {
                target: remove_slot_1,
                kind: UiEventKind::Click,
                payload: None,
            }],
        });

        assert!(!preview.preview_text().contains("Item A (id=0)"));
        assert!(preview.preview_text().contains("Item B (id=1)"));
    }
}
