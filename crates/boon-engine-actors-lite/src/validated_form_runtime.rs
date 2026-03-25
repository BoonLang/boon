use crate::host_view_preview::HostViewPreviewApp;
use crate::input_form_runtime::{
    FormInputBinding, FormInputDispatch, FormInputEvent, InputFormRuntime,
};
use crate::ir::{SinkPortId, SourcePortId};
use crate::mapped_click_runtime::MappedClickRuntime;
use crate::preview_shell::render_preview_shell;
use boon::platform::browser::kernel::KernelValue;
use boon::zoon::*;
use boon_renderer_zoon::FakeRenderState;
use boon_scene::{UiEventBatch, UiNode};
use std::collections::BTreeMap;

pub struct ValidatedFormDispatch {
    pub inputs_changed: bool,
    pub input_events: Vec<FormInputEvent>,
    pub clicked_ports: Vec<SourcePortId>,
}

pub struct ValidatedFormRuntime<const N: usize> {
    app: HostViewPreviewApp,
    inputs: InputFormRuntime<N>,
    clicks: MappedClickRuntime,
}

impl<const N: usize> ValidatedFormRuntime<N> {
    #[must_use]
    pub fn new(
        host_view: crate::bridge::HostViewIr,
        sink_values: BTreeMap<SinkPortId, KernelValue>,
        bindings: [FormInputBinding; N],
        click_ports: impl IntoIterator<Item = SourcePortId>,
    ) -> Self {
        Self {
            app: HostViewPreviewApp::new(host_view, sink_values),
            inputs: InputFormRuntime::new(bindings),
            clicks: MappedClickRuntime::new(click_ports),
        }
    }

    pub fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> ValidatedFormDispatch {
        let _ = self.app.render_root();
        let FormInputDispatch { changed, events } =
            self.inputs.dispatch_ui_events(&self.app, &batch);
        let clicked_ports = self.clicks.dispatch_clicks(&self.app, batch);
        ValidatedFormDispatch {
            inputs_changed: changed,
            input_events: events,
            clicked_ports,
        }
    }

    #[must_use]
    pub fn input(&self, index: usize) -> &str {
        self.inputs.input(index)
    }

    pub fn set_sink_value(&mut self, sink: SinkPortId, value: KernelValue) {
        self.app.set_sink_value(sink, value);
    }

    #[must_use]
    pub fn sink_value(&self, sink: SinkPortId) -> Option<&KernelValue> {
        self.app.sink_value(sink)
    }

    #[must_use]
    pub fn app(&self) -> &HostViewPreviewApp {
        &self.app
    }

    #[must_use]
    pub fn render_root(&mut self) -> UiNode {
        self.app.render_root()
    }

    #[must_use]
    pub fn render_snapshot(&mut self) -> (UiNode, FakeRenderState) {
        self.app.render_snapshot()
    }

    #[must_use]
    pub fn preview_text(&mut self) -> String {
        self.app.preview_text()
    }
}

pub fn render_validated_form_preview<const N: usize>(
    runtime: ValidatedFormRuntime<N>,
    on_events: impl Fn(&mut ValidatedFormRuntime<N>, UiEventBatch) + 'static,
) -> impl Element {
    render_preview_shell(runtime, on_events, |runtime| runtime.render_snapshot())
}
