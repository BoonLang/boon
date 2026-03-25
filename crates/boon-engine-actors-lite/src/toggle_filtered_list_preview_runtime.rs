use crate::bridge::HostViewIr;
use crate::host_view_preview::HostViewPreviewApp;
use crate::ir::{SinkPortId, SourcePortId};
use crate::mapped_list_view_runtime::MappedListViewRuntime;
use crate::preview_shell::render_preview_shell;
use crate::selected_filter_click_runtime::SelectedFilterClickRuntime;
use boon::platform::browser::kernel::KernelValue;
use boon::zoon::*;
use boon_renderer_zoon::FakeRenderState;
use boon_scene::{UiEventBatch, UiNode};
use std::collections::BTreeMap;

pub trait ToggleFilteredListProjection<T> {
    fn host_view(&self) -> &HostViewIr;

    fn toggle_port(&self) -> SourcePortId;

    fn initial_sink_values(
        &self,
        items: &MappedListViewRuntime<T>,
    ) -> BTreeMap<SinkPortId, KernelValue>;

    fn refresh_sink_values(
        &self,
        app: &mut HostViewPreviewApp,
        filter_enabled: bool,
        items: &MappedListViewRuntime<T>,
    );
}

pub struct ToggleFilteredListPreviewRuntime<T, P> {
    projection: P,
    app: HostViewPreviewApp,
    filter_enabled: SelectedFilterClickRuntime<bool>,
    items: MappedListViewRuntime<T>,
}

impl<T, P> ToggleFilteredListPreviewRuntime<T, P>
where
    P: ToggleFilteredListProjection<T>,
{
    #[must_use]
    pub fn new(
        projection: P,
        initial_items: impl IntoIterator<Item = (u64, T)>,
        next_id: u64,
    ) -> Self {
        let toggle_port = projection.toggle_port();
        let items = MappedListViewRuntime::new(initial_items, next_id);
        let app = HostViewPreviewApp::new(
            projection.host_view().clone(),
            projection.initial_sink_values(&items),
        );

        Self {
            projection,
            app,
            filter_enabled: SelectedFilterClickRuntime::new(false, [toggle_port]),
            items,
        }
    }

    pub fn click_toggle(&mut self) {
        self.apply_clicked_ports(vec![self.projection.toggle_port()]);
    }

    pub fn dispatch_ui_events(&mut self, batch: UiEventBatch) {
        let _ = self.render_root();
        let toggle_port = self.projection.toggle_port();
        let changed = self
            .filter_enabled
            .dispatch_ui_events(&self.app, batch, |filter, port| {
                if port == toggle_port {
                    filter.toggle()
                } else {
                    false
                }
            });
        if changed {
            self.projection.refresh_sink_values(
                &mut self.app,
                self.filter_enabled.current(),
                &self.items,
            );
        }
    }

    #[must_use]
    pub fn render_root(&mut self) -> UiNode {
        self.app.render_root()
    }

    pub fn render_snapshot(&mut self) -> (UiNode, FakeRenderState) {
        self.app.render_snapshot()
    }

    #[must_use]
    pub fn preview_text(&mut self) -> String {
        self.app.preview_text()
    }

    #[must_use]
    pub fn app(&self) -> &HostViewPreviewApp {
        &self.app
    }

    #[must_use]
    pub fn filter_enabled(&self) -> bool {
        self.filter_enabled.current()
    }

    #[must_use]
    pub fn items(&self) -> &MappedListViewRuntime<T> {
        &self.items
    }

    fn apply_clicked_ports(&mut self, clicked_ports: Vec<SourcePortId>) {
        let toggle_port = self.projection.toggle_port();
        let changed = self
            .filter_enabled
            .apply_clicked_ports(clicked_ports, |filter, port| {
                if port == toggle_port {
                    filter.toggle()
                } else {
                    false
                }
            });
        if changed {
            self.projection.refresh_sink_values(
                &mut self.app,
                self.filter_enabled.current(),
                &self.items,
            );
        }
    }
}

pub fn render_toggle_filtered_list_preview<
    T: 'static,
    P: ToggleFilteredListProjection<T> + 'static,
>(
    preview: ToggleFilteredListPreviewRuntime<T, P>,
) -> impl Element {
    render_preview_shell(
        preview,
        |preview, batch| preview.dispatch_ui_events(batch),
        |preview| preview.render_snapshot(),
    )
}
