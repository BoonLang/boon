use crate::append_list_runtime::AppendListRuntime;
use crate::bridge::HostViewIr;
use crate::host_view_preview::HostViewPreviewApp;
use crate::ir::SinkPortId;
use crate::preview_shell::render_preview_shell;
use boon::platform::browser::kernel::KernelValue;
use boon::zoon::*;
use boon_renderer_zoon::FakeRenderState;
use boon_scene::{UiEventBatch, UiNode};
use std::collections::BTreeMap;

pub trait AppendListProjection {
    fn host_view(&self) -> &HostViewIr;

    fn initial_sink_values(
        &self,
        input: &str,
        items: &[String],
    ) -> BTreeMap<SinkPortId, KernelValue>;

    fn refresh_sink_values(&self, app: &mut HostViewPreviewApp, input: &str, items: &[String]);
}

pub struct AppendListPreviewRuntime<P> {
    projection: P,
    app: HostViewPreviewApp,
    state: AppendListRuntime,
}

impl<P: AppendListProjection> AppendListPreviewRuntime<P> {
    #[must_use]
    pub fn new(projection: P, state: AppendListRuntime, initial_items: &[String]) -> Self {
        let app = HostViewPreviewApp::new(
            projection.host_view().clone(),
            projection.initial_sink_values("", initial_items),
        );
        Self {
            projection,
            app,
            state,
        }
    }

    pub fn dispatch_ui_events(&mut self, batch: UiEventBatch) {
        let _ = self.render_root();
        if self.state.dispatch_ui_events(&self.app, batch) {
            self.projection.refresh_sink_values(
                &mut self.app,
                self.state.input(),
                self.state.items(),
            );
        }
    }

    #[must_use]
    pub fn render_root(&mut self) -> UiNode {
        self.app.render_root()
    }

    #[must_use]
    pub fn preview_text(&mut self) -> String {
        self.app.preview_text()
    }

    pub fn render_snapshot(&mut self) -> (UiNode, FakeRenderState) {
        self.app.render_snapshot()
    }

    #[must_use]
    pub fn app(&self) -> &HostViewPreviewApp {
        &self.app
    }

    #[must_use]
    pub fn state(&self) -> &AppendListRuntime {
        &self.state
    }
}

pub fn render_append_list_preview<P: AppendListProjection + 'static>(
    preview: AppendListPreviewRuntime<P>,
) -> impl Element {
    render_preview_shell(
        preview,
        |preview, batch| preview.dispatch_ui_events(batch),
        |preview| preview.render_snapshot(),
    )
}
