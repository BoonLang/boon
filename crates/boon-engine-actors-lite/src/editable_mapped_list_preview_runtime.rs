use crate::bridge::HostViewIr;
use crate::editable_mapped_list_runtime::EditableMappedListRuntime;
use crate::host_view_preview::HostViewPreviewApp;
use crate::ir::{SinkPortId, SourcePortId};
use crate::mapped_click_runtime::MappedClickRuntime;
use crate::mapped_list_runtime::MappedListItem;
use crate::preview_shell::render_preview_shell;
use boon::platform::browser::kernel::KernelValue;
use boon::zoon::*;
use boon_renderer_zoon::FakeRenderState;
use boon_scene::{UiEventBatch, UiNode};
use std::collections::BTreeMap;

pub trait EditableMappedListProjection<T, const INPUTS: usize, const ROWS: usize> {
    fn host_view(&self) -> &HostViewIr;

    fn initial_sink_values(
        &self,
        state: &EditableMappedListRuntime<T, INPUTS, ROWS>,
    ) -> BTreeMap<SinkPortId, KernelValue>;

    fn refresh_sink_values(
        &self,
        app: &mut HostViewPreviewApp,
        state: &EditableMappedListRuntime<T, INPUTS, ROWS>,
    );
}

pub struct EditableMappedListPreviewRuntime<
    T,
    P,
    const INPUTS: usize,
    const ROWS: usize,
    const BUTTONS: usize,
> {
    projection: P,
    app: HostViewPreviewApp,
    state: EditableMappedListRuntime<T, INPUTS, ROWS>,
    button_clicks: MappedClickRuntime,
}

impl<T, P, const INPUTS: usize, const ROWS: usize, const BUTTONS: usize>
    EditableMappedListPreviewRuntime<T, P, INPUTS, ROWS, BUTTONS>
where
    P: EditableMappedListProjection<T, INPUTS, ROWS>,
{
    #[must_use]
    pub fn new(
        projection: P,
        state: EditableMappedListRuntime<T, INPUTS, ROWS>,
        button_ports: [SourcePortId; BUTTONS],
    ) -> Self {
        Self {
            app: HostViewPreviewApp::new(
                projection.host_view().clone(),
                projection.initial_sink_values(&state),
            ),
            projection,
            state,
            button_clicks: MappedClickRuntime::new(button_ports),
        }
    }

    pub fn dispatch_ui_events(
        &mut self,
        batch: UiEventBatch,
        is_visible: impl Fn(&MappedListItem<T>) -> bool,
        mut on_button_clicks: impl FnMut(
            &mut EditableMappedListRuntime<T, INPUTS, ROWS>,
            Vec<SourcePortId>,
        ) -> bool,
    ) {
        let _ = self.render_root();
        let mut changed = self.state.dispatch_input_events(&self.app, &batch);

        let button_clicked = self.button_clicks.dispatch_clicks(&self.app, batch.clone());
        if !button_clicked.is_empty() {
            changed |= on_button_clicks(&mut self.state, button_clicked);
        }

        changed |= self.state.dispatch_row_clicks(&self.app, batch, is_visible);
        if changed {
            self.projection
                .refresh_sink_values(&mut self.app, &self.state);
        }
    }

    pub fn refresh(&mut self) {
        self.projection
            .refresh_sink_values(&mut self.app, &self.state);
    }

    #[must_use]
    pub fn app(&self) -> &HostViewPreviewApp {
        &self.app
    }

    #[must_use]
    pub fn state(&self) -> &EditableMappedListRuntime<T, INPUTS, ROWS> {
        &self.state
    }

    pub fn state_mut(&mut self) -> &mut EditableMappedListRuntime<T, INPUTS, ROWS> {
        &mut self.state
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
}

pub fn render_editable_mapped_list_preview<
    T: 'static,
    P: EditableMappedListProjection<T, INPUTS, ROWS> + 'static,
    const INPUTS: usize,
    const ROWS: usize,
    const BUTTONS: usize,
>(
    preview: EditableMappedListPreviewRuntime<T, P, INPUTS, ROWS, BUTTONS>,
    dispatch: impl Fn(&mut EditableMappedListPreviewRuntime<T, P, INPUTS, ROWS, BUTTONS>, UiEventBatch)
    + 'static,
) -> impl Element {
    render_preview_shell(preview, dispatch, |preview| preview.render_snapshot())
}
