use crate::host_view_preview::HostViewPreviewApp;
use crate::ir::SinkPortId;
use crate::lower::{ListObjectStateProgram, try_lower_list_object_state};
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

pub struct ListObjectStatePreview {
    program: ListObjectStateProgram,
    app: HostViewPreviewApp,
    counts: MappedItemStateRuntime<u32, 3>,
}

impl ListObjectStatePreview {
    pub fn new(source: &str) -> Result<Self, String> {
        let program = try_lower_list_object_state(source)?;
        let app = HostViewPreviewApp::new(program.host_view.clone(), initial_sink_values(&program));
        let counts = MappedItemStateRuntime::new(program.press_ports, [0, 0, 0]);

        Ok(Self {
            program,
            app,
            counts,
        })
    }

    pub fn click_button(&mut self, index: usize) {
        if self.counts.apply_clicked_ports(
            vec![self.program.press_ports[index]],
            |_index, count| {
                *count += 1;
            },
        ) {
            refresh_sink_values(&mut self.app, &self.program, self.counts.items());
        }
    }

    pub fn dispatch_ui_events(&mut self, batch: UiEventBatch) {
        let _ = self.render_root();
        if self
            .counts
            .dispatch_ui_events(&self.app, batch, |_index, count| {
                *count += 1;
            })
        {
            refresh_sink_values(&mut self.app, &self.program, self.counts.items());
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
    program: &ListObjectStateProgram,
) -> BTreeMap<crate::ir::SinkPortId, KernelValue> {
    let mut sink_values = BTreeMap::new();
    sink_values.insert(
        SinkPortId(89),
        KernelValue::from("Click each button - counts should be independent"),
    );
    for sink in program.count_sinks {
        sink_values.insert(sink, KernelValue::from("Count: 0"));
    }
    sink_values
}

fn refresh_sink_values(
    app: &mut HostViewPreviewApp,
    program: &ListObjectStateProgram,
    counts: &[u32; 3],
) {
    for (sink, count) in program.count_sinks.iter().zip(counts.iter()) {
        app.set_sink_value(*sink, KernelValue::from(format!("Count: {count}")));
    }
}

pub fn render_list_object_state_preview(preview: ListObjectStatePreview) -> impl Element {
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
            Some(render_snapshot_root_with_handlers(
                &RenderRoot::UiTree(root),
                &state,
                &handlers,
            ))
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_object_state_preview_keeps_counts_independent() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/list_object_state/list_object_state.bn"
        );
        let mut preview = ListObjectStatePreview::new(source).expect("list_object_state preview");
        assert_eq!(
            preview.preview_text(),
            "Click each button - counts should be independentClick meCount: 0Click meCount: 0Click meCount: 0"
        );

        preview.click_button(0);
        preview.click_button(1);
        preview.click_button(1);
        preview.click_button(2);

        assert_eq!(
            preview.preview_text(),
            "Click each button - counts should be independentClick meCount: 1Click meCount: 2Click meCount: 1"
        );
    }
}
