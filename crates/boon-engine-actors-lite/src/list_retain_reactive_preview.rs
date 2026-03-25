use crate::filtered_list_view::filtered_list_with_filter;
use crate::lower::{ListRetainReactiveProgram, try_lower_list_retain_reactive};
use crate::mapped_list_view_runtime::MappedListViewRuntime;
use crate::toggle_filtered_list_preview_runtime::{
    ToggleFilteredListPreviewRuntime, ToggleFilteredListProjection,
    render_toggle_filtered_list_preview,
};
use boon::platform::browser::kernel::KernelValue;
use boon::zoon::*;
use std::collections::BTreeMap;

const VALUES: [i64; 6] = [1, 2, 3, 4, 5, 6];

struct ListRetainReactiveProjection {
    program: ListRetainReactiveProgram,
}

impl ToggleFilteredListProjection<i64> for ListRetainReactiveProjection {
    fn host_view(&self) -> &crate::bridge::HostViewIr {
        &self.program.host_view
    }

    fn toggle_port(&self) -> crate::ir::SourcePortId {
        self.program.toggle_port
    }

    fn initial_sink_values(
        &self,
        items: &MappedListViewRuntime<i64>,
    ) -> BTreeMap<crate::ir::SinkPortId, KernelValue> {
        initial_sink_values(&self.program, items)
    }

    fn refresh_sink_values(
        &self,
        app: &mut crate::host_view_preview::HostViewPreviewApp,
        filter_enabled: bool,
        items: &MappedListViewRuntime<i64>,
    ) {
        refresh_sink_values(app, &self.program, filter_enabled, items);
    }
}

pub struct ListRetainReactivePreview {
    runtime: ToggleFilteredListPreviewRuntime<i64, ListRetainReactiveProjection>,
}

impl ListRetainReactivePreview {
    pub fn new(source: &str) -> Result<Self, String> {
        let program = try_lower_list_retain_reactive(source)?;
        let runtime = ToggleFilteredListPreviewRuntime::new(
            ListRetainReactiveProjection {
                program: program.clone(),
            },
            VALUES
                .into_iter()
                .enumerate()
                .map(|(index, value)| (index as u64, value)),
            VALUES.len() as u64,
        );

        Ok(Self { runtime })
    }

    pub fn click_toggle(&mut self) {
        self.runtime.click_toggle();
    }

    pub fn dispatch_ui_events(&mut self, batch: boon_scene::UiEventBatch) {
        self.runtime.dispatch_ui_events(batch);
    }

    #[must_use]
    pub fn render_root(&mut self) -> boon_scene::UiNode {
        self.runtime.render_root()
    }

    #[must_use]
    pub fn preview_text(&mut self) -> String {
        self.runtime.preview_text()
    }
}

fn initial_sink_values(
    program: &ListRetainReactiveProgram,
    items: &MappedListViewRuntime<i64>,
) -> BTreeMap<crate::ir::SinkPortId, KernelValue> {
    let mut sink_values = BTreeMap::new();
    sink_values.insert(program.mode_sink, KernelValue::from("show_even: False"));
    let visible_items = filtered_list_with_filter(items, false, |show_even, item| {
        !*show_even || item.value % 2 == 0
    });
    sink_values.insert(
        program.count_sink,
        KernelValue::from(format!("Filtered count: {}", visible_items.iter().count())),
    );
    visible_items.project_into_map(
        &mut sink_values,
        &program.item_sinks,
        |item| KernelValue::from(item.value as f64),
        KernelValue::from(""),
    );
    sink_values
}

fn refresh_sink_values(
    app: &mut crate::host_view_preview::HostViewPreviewApp,
    program: &ListRetainReactiveProgram,
    show_even: bool,
    items: &MappedListViewRuntime<i64>,
) {
    let visible_items = filtered_list_with_filter(items, show_even, |show_even, item| {
        !*show_even || item.value % 2 == 0
    });
    let count = visible_items.iter().count();

    app.set_sink_value(
        program.mode_sink,
        KernelValue::from(if show_even {
            "show_even: True"
        } else {
            "show_even: False"
        }),
    );
    app.set_sink_value(
        program.count_sink,
        KernelValue::from(format!("Filtered count: {count}")),
    );
    visible_items.project_into_app(
        app,
        &program.item_sinks,
        |item| KernelValue::from(item.value as f64),
        KernelValue::from(""),
    );
}

pub fn render_list_retain_reactive_preview(preview: ListRetainReactivePreview) -> impl Element {
    render_toggle_filtered_list_preview(preview.runtime)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
