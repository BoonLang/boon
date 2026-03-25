use crate::host_view_preview::HostViewPreviewApp;
use crate::lower::{FilterCheckboxBugProgram, try_lower_filter_checkbox_bug};
use crate::mapped_item_state_runtime::MappedItemStateRuntime;
use crate::selected_filter_click_runtime::SelectedFilterClickRuntime;
use boon::platform::browser::kernel::KernelValue;
use boon::zoon::*;
use boon_renderer_zoon::{
    FakeRenderState, RenderInteractionHandlers, render_snapshot_root_with_handlers,
};
use boon_scene::{RenderRoot, UiEventBatch, UiFactBatch, UiNode};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FilterMode {
    All,
    Active,
}

struct FilterCheckboxItem {
    name: &'static str,
    checked: bool,
}

pub struct FilterCheckboxBugPreview {
    program: FilterCheckboxBugProgram,
    app: HostViewPreviewApp,
    filter: SelectedFilterClickRuntime<FilterMode>,
    items: MappedItemStateRuntime<FilterCheckboxItem, 2>,
}

impl FilterCheckboxBugPreview {
    pub fn new(source: &str) -> Result<Self, String> {
        let program = try_lower_filter_checkbox_bug(source)?;
        let filter_ports = [program.filter_all_port, program.filter_active_port];
        let items = [
            FilterCheckboxItem {
                name: "Item A",
                checked: false,
            },
            FilterCheckboxItem {
                name: "Item B",
                checked: false,
            },
        ];
        let app = HostViewPreviewApp::new(
            program.host_view.clone(),
            initial_sink_values(&program, &items),
        );
        let items = MappedItemStateRuntime::new(program.checkbox_ports, items);

        Ok(Self {
            program,
            app,
            filter: SelectedFilterClickRuntime::new(FilterMode::All, filter_ports),
            items,
        })
    }

    pub fn dispatch_ui_events(&mut self, batch: UiEventBatch) {
        let _ = self.render_root();
        let filter_changed =
            self.filter
                .dispatch_ui_events(&self.app, batch.clone(), |filter, port| {
                    if port == self.program.filter_all_port {
                        filter.select(FilterMode::All)
                    } else if port == self.program.filter_active_port {
                        filter.select(FilterMode::Active)
                    } else {
                        false
                    }
                });
        let item_changed = self
            .items
            .dispatch_ui_events(&self.app, batch, |_index, item| {
                item.checked = !item.checked;
            });
        if filter_changed || item_changed {
            refresh_sink_values(
                &mut self.app,
                &self.program,
                self.filter.current(),
                self.items.items(),
            );
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
    program: &FilterCheckboxBugProgram,
    items: &[FilterCheckboxItem; 2],
) -> BTreeMap<crate::ir::SinkPortId, KernelValue> {
    let mut sink_values = BTreeMap::new();
    sink_values.insert(program.filter_sink, KernelValue::from("Filter: All"));
    sink_values.insert(
        program.footer_sink,
        KernelValue::from("Test: Click Active, All, then checkbox 3x"),
    );
    for (sink, item) in program.checkbox_sinks.iter().zip(items.iter()) {
        sink_values.insert(*sink, KernelValue::Bool(item.checked));
    }
    for (sink, item) in program.item_label_sinks.iter().zip(items.iter()) {
        sink_values.insert(
            *sink,
            KernelValue::from(format!("{} (ALL) - checked: {}", item.name, item.checked)),
        );
    }
    sink_values
}

fn refresh_sink_values(
    app: &mut HostViewPreviewApp,
    program: &FilterCheckboxBugProgram,
    filter: FilterMode,
    items: &[FilterCheckboxItem; 2],
) {
    let filter_text = match filter {
        FilterMode::All => "Filter: All",
        FilterMode::Active => "Filter: Active",
    };
    let view_label = match filter {
        FilterMode::All => "ALL",
        FilterMode::Active => "ACTIVE",
    };

    app.set_sink_value(program.filter_sink, KernelValue::from(filter_text));
    app.set_sink_value(
        program.footer_sink,
        KernelValue::from("Test: Click Active, All, then checkbox 3x"),
    );
    for (sink, item) in program.checkbox_sinks.iter().zip(items.iter()) {
        app.set_sink_value(*sink, KernelValue::Bool(item.checked));
    }
    for (sink, item) in program.item_label_sinks.iter().zip(items.iter()) {
        app.set_sink_value(
            *sink,
            KernelValue::from(format!(
                "{} ({view_label}) - checked: {}",
                item.name, item.checked
            )),
        );
    }
}

pub fn render_filter_checkbox_bug_preview(preview: FilterCheckboxBugPreview) -> impl Element {
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
    fn filter_checkbox_bug_preview_toggles_after_filter_switching() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/filter_checkbox_bug/filter_checkbox_bug.bn"
        );
        let mut preview =
            FilterCheckboxBugPreview::new(source).expect("filter_checkbox_bug preview");
        assert_eq!(
            preview.preview_text(),
            "Filter: AllAllActiveItem A (ALL) - checked: falseItem B (ALL) - checked: falseTest: Click Active, All, then checkbox 3x"
        );

        let _ = preview.render_root();
        let active_port = preview
            .app
            .event_port_for_source(preview.program.filter_active_port)
            .expect("active port");
        let all_port = preview
            .app
            .event_port_for_source(preview.program.filter_all_port)
            .expect("all port");
        let checkbox_port = preview
            .app
            .event_port_for_source(preview.program.checkbox_ports[0])
            .expect("checkbox port");

        for target in [
            active_port,
            all_port,
            checkbox_port,
            checkbox_port,
            checkbox_port,
        ] {
            preview.dispatch_ui_events(UiEventBatch {
                events: vec![boon_scene::UiEvent {
                    target,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            });
        }

        assert_eq!(
            preview.preview_text(),
            "Filter: AllAllActiveItem A (ALL) - checked: trueItem B (ALL) - checked: falseTest: Click Active, All, then checkbox 3x"
        );
    }
}
