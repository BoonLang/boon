use crate::host_view_preview::HostViewPreviewApp;
use crate::interactive_preview::{InteractivePreview, render_interactive_preview};
use crate::lower::{
    ThenProgram, WhenProgram, WhileProgram, try_lower_then, try_lower_when, try_lower_while,
};
use boon::platform::browser::kernel::KernelValue;
use boon::zoon::*;
use boon_renderer_zoon::FakeRenderState;
use boon_scene::{RenderRoot, UiEventBatch, UiEventKind};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operation {
    Addition,
    Subtraction,
}

impl Operation {
    fn apply(self, input_a: i64, input_b: i64) -> i64 {
        match self {
            Self::Addition => input_a + input_b,
            Self::Subtraction => input_a - input_b,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TimedInputs {
    tick_count: u64,
    input_a: i64,
    input_b: i64,
}

impl TimedInputs {
    fn tick(&mut self) {
        self.tick_count += 1;
        self.input_a += 1;
        if self.tick_count % 2 == 0 {
            self.input_b += 10;
        }
    }
}

pub struct ThenPreview {
    program: ThenProgram,
    inputs: TimedInputs,
    result: Option<i64>,
    app: HostViewPreviewApp,
}

impl ThenPreview {
    pub fn new(source: &str) -> Result<Self, String> {
        let program = try_lower_then(source)?;
        let inputs = TimedInputs::default();
        let app = HostViewPreviewApp::new(
            program.host_view.clone(),
            then_sink_values(&program, inputs, None),
        );
        Ok(Self {
            program,
            inputs,
            result: None,
            app,
        })
    }

    #[must_use]
    pub fn app(&self) -> &HostViewPreviewApp {
        &self.app
    }

    #[must_use]
    pub fn preview_text(&mut self) -> String {
        self.app.preview_text()
    }

    fn set_input_sinks(&mut self) {
        self.app.set_sink_value(
            self.program.input_a_sink,
            KernelValue::from(format!("A: {}", self.inputs.input_a)),
        );
        self.app.set_sink_value(
            self.program.input_b_sink,
            KernelValue::from(format!("B: {}", self.inputs.input_b)),
        );
    }

    fn set_result(&mut self, result: Option<i64>) -> bool {
        if self.result == result {
            return false;
        }
        self.result = result;
        self.app.set_sink_value(
            self.program.result_sink,
            KernelValue::from(result.map_or_else(String::new, |value| value.to_string())),
        );
        true
    }
}

impl InteractivePreview for ThenPreview {
    fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool {
        let tick_port = self.app.event_port_for_source(self.program.tick_port);
        let addition_port = self
            .app
            .event_port_for_source(self.program.addition_press_port);
        let mut changed = false;

        for event in batch.events {
            if Some(event.target) == tick_port && matches!(event.kind, UiEventKind::Custom(_)) {
                self.inputs.tick();
                self.set_input_sinks();
                changed = true;
            } else if Some(event.target) == addition_port && event.kind == UiEventKind::Click {
                changed |= self.set_result(Some(self.inputs.input_a + self.inputs.input_b));
            }
        }

        changed
    }

    fn dispatch_ui_facts(&mut self, _batch: boon_scene::UiFactBatch) -> bool {
        false
    }

    fn render_snapshot(&mut self) -> (RenderRoot, FakeRenderState) {
        let (root, state) = self.app.render_snapshot();
        (RenderRoot::UiTree(root), state)
    }
}

pub struct WhenPreview {
    program: WhenProgram,
    inputs: TimedInputs,
    operation: Option<Operation>,
    result: Option<i64>,
    app: HostViewPreviewApp,
}

impl WhenPreview {
    pub fn new(source: &str) -> Result<Self, String> {
        let program = try_lower_when(source)?;
        let inputs = TimedInputs::default();
        let app = HostViewPreviewApp::new(
            program.host_view.clone(),
            when_sink_values(&program, inputs, None),
        );
        Ok(Self {
            program,
            inputs,
            operation: None,
            result: None,
            app,
        })
    }

    #[must_use]
    pub fn app(&self) -> &HostViewPreviewApp {
        &self.app
    }

    #[must_use]
    pub fn preview_text(&mut self) -> String {
        self.app.preview_text()
    }

    fn set_input_sinks(&mut self) {
        self.app.set_sink_value(
            self.program.input_a_sink,
            KernelValue::from(format!("A: {}", self.inputs.input_a)),
        );
        self.app.set_sink_value(
            self.program.input_b_sink,
            KernelValue::from(format!("B: {}", self.inputs.input_b)),
        );
    }

    fn set_result(&mut self, result: Option<i64>) -> bool {
        if self.result == result {
            return false;
        }
        self.result = result;
        self.app.set_sink_value(
            self.program.result_sink,
            KernelValue::from(result.map_or_else(String::new, |value| value.to_string())),
        );
        true
    }

    fn select_operation(&mut self, operation: Operation) -> bool {
        self.operation = Some(operation);
        self.set_result(Some(
            operation.apply(self.inputs.input_a, self.inputs.input_b),
        ))
    }
}

impl InteractivePreview for WhenPreview {
    fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool {
        let tick_port = self.app.event_port_for_source(self.program.tick_port);
        let addition_port = self
            .app
            .event_port_for_source(self.program.addition_press_port);
        let subtraction_port = self
            .app
            .event_port_for_source(self.program.subtraction_press_port);
        let mut changed = false;

        for event in batch.events {
            if Some(event.target) == tick_port && matches!(event.kind, UiEventKind::Custom(_)) {
                self.inputs.tick();
                self.set_input_sinks();
                changed = true;
            } else if Some(event.target) == addition_port && event.kind == UiEventKind::Click {
                changed |= self.select_operation(Operation::Addition);
            } else if Some(event.target) == subtraction_port && event.kind == UiEventKind::Click {
                changed |= self.select_operation(Operation::Subtraction);
            }
        }

        changed
    }

    fn dispatch_ui_facts(&mut self, _batch: boon_scene::UiFactBatch) -> bool {
        false
    }

    fn render_snapshot(&mut self) -> (RenderRoot, FakeRenderState) {
        let (root, state) = self.app.render_snapshot();
        (RenderRoot::UiTree(root), state)
    }
}

pub struct WhilePreview {
    program: WhileProgram,
    inputs: TimedInputs,
    operation: Option<Operation>,
    result: Option<i64>,
    app: HostViewPreviewApp,
}

impl WhilePreview {
    pub fn new(source: &str) -> Result<Self, String> {
        let program = try_lower_while(source)?;
        let inputs = TimedInputs::default();
        let app = HostViewPreviewApp::new(
            program.host_view.clone(),
            while_sink_values(&program, inputs, None),
        );
        Ok(Self {
            program,
            inputs,
            operation: None,
            result: None,
            app,
        })
    }

    #[must_use]
    pub fn app(&self) -> &HostViewPreviewApp {
        &self.app
    }

    #[must_use]
    pub fn preview_text(&mut self) -> String {
        self.app.preview_text()
    }

    fn set_input_sinks(&mut self) {
        self.app.set_sink_value(
            self.program.input_a_sink,
            KernelValue::from(format!("A: {}", self.inputs.input_a)),
        );
        self.app.set_sink_value(
            self.program.input_b_sink,
            KernelValue::from(format!("B: {}", self.inputs.input_b)),
        );
    }

    fn set_result(&mut self, result: Option<i64>) -> bool {
        if self.result == result {
            return false;
        }
        self.result = result;
        self.app.set_sink_value(
            self.program.result_sink,
            KernelValue::from(result.map_or_else(String::new, |value| value.to_string())),
        );
        true
    }

    fn recompute_result(&mut self) -> bool {
        self.set_result(
            self.operation
                .map(|operation| operation.apply(self.inputs.input_a, self.inputs.input_b)),
        )
    }
}

impl InteractivePreview for WhilePreview {
    fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool {
        let tick_port = self.app.event_port_for_source(self.program.tick_port);
        let addition_port = self
            .app
            .event_port_for_source(self.program.addition_press_port);
        let subtraction_port = self
            .app
            .event_port_for_source(self.program.subtraction_press_port);
        let mut changed = false;

        for event in batch.events {
            if Some(event.target) == tick_port && matches!(event.kind, UiEventKind::Custom(_)) {
                self.inputs.tick();
                self.set_input_sinks();
                changed = true;
                changed |= self.recompute_result();
            } else if Some(event.target) == addition_port && event.kind == UiEventKind::Click {
                self.operation = Some(Operation::Addition);
                changed |= self.recompute_result();
            } else if Some(event.target) == subtraction_port && event.kind == UiEventKind::Click {
                self.operation = Some(Operation::Subtraction);
                changed |= self.recompute_result();
            }
        }

        changed
    }

    fn dispatch_ui_facts(&mut self, _batch: boon_scene::UiFactBatch) -> bool {
        false
    }

    fn render_snapshot(&mut self) -> (RenderRoot, FakeRenderState) {
        let (root, state) = self.app.render_snapshot();
        (RenderRoot::UiTree(root), state)
    }
}

pub fn render_then_preview(preview: ThenPreview) -> impl Element {
    render_interactive_preview(preview)
}

pub fn render_when_preview(preview: WhenPreview) -> impl Element {
    render_interactive_preview(preview)
}

pub fn render_while_preview(preview: WhilePreview) -> impl Element {
    render_interactive_preview(preview)
}

fn then_sink_values(
    program: &ThenProgram,
    inputs: TimedInputs,
    result: Option<i64>,
) -> BTreeMap<crate::ir::SinkPortId, KernelValue> {
    BTreeMap::from([
        (
            program.input_a_sink,
            KernelValue::from(format!("A: {}", inputs.input_a)),
        ),
        (
            program.input_b_sink,
            KernelValue::from(format!("B: {}", inputs.input_b)),
        ),
        (
            program.result_sink,
            KernelValue::from(result.map_or_else(String::new, |value| value.to_string())),
        ),
    ])
}

fn when_sink_values(
    program: &WhenProgram,
    inputs: TimedInputs,
    result: Option<i64>,
) -> BTreeMap<crate::ir::SinkPortId, KernelValue> {
    BTreeMap::from([
        (
            program.input_a_sink,
            KernelValue::from(format!("A: {}", inputs.input_a)),
        ),
        (
            program.input_b_sink,
            KernelValue::from(format!("B: {}", inputs.input_b)),
        ),
        (
            program.result_sink,
            KernelValue::from(result.map_or_else(String::new, |value| value.to_string())),
        ),
    ])
}

fn while_sink_values(
    program: &WhileProgram,
    inputs: TimedInputs,
    result: Option<i64>,
) -> BTreeMap<crate::ir::SinkPortId, KernelValue> {
    BTreeMap::from([
        (
            program.input_a_sink,
            KernelValue::from(format!("A: {}", inputs.input_a)),
        ),
        (
            program.input_b_sink,
            KernelValue::from(format!("B: {}", inputs.input_b)),
        ),
        (
            program.result_sink,
            KernelValue::from(result.map_or_else(String::new, |value| value.to_string())),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use boon_scene::UiEvent;

    fn click(target: boon_scene::EventPortId) -> UiEventBatch {
        UiEventBatch {
            events: vec![UiEvent {
                target,
                kind: UiEventKind::Click,
                payload: None,
            }],
        }
    }

    fn tick(target: boon_scene::EventPortId) -> UiEventBatch {
        UiEventBatch {
            events: vec![UiEvent {
                target,
                kind: UiEventKind::Custom("timer:500".to_string()),
                payload: None,
            }],
        }
    }

    #[test]
    fn then_preview_keeps_sum_stable_until_next_press() {
        let source = include_str!("../../../playground/frontend/src/examples/then/then.bn");
        let mut preview = ThenPreview::new(source).expect("then preview");
        assert_eq!(preview.preview_text(), "A: 0B: 0A + B");

        let _ = preview.render_snapshot();
        let tick_port = preview
            .app()
            .event_port_for_source(preview.program.tick_port)
            .expect("tick");
        let addition = preview
            .app()
            .event_port_for_source(preview.program.addition_press_port)
            .expect("addition");

        preview.dispatch_ui_events(tick(tick_port));
        preview.dispatch_ui_events(tick(tick_port));
        assert_eq!(preview.preview_text(), "A: 2B: 10A + B");

        preview.dispatch_ui_events(click(addition));
        assert_eq!(preview.preview_text(), "A: 2B: 10A + B12");

        preview.dispatch_ui_events(tick(tick_port));
        preview.dispatch_ui_events(tick(tick_port));
        assert_eq!(preview.preview_text(), "A: 4B: 20A + B12");

        preview.dispatch_ui_events(click(addition));
        assert_eq!(preview.preview_text(), "A: 4B: 20A + B24");
    }

    #[test]
    fn when_preview_keeps_result_stable_until_operation_changes() {
        let source = include_str!("../../../playground/frontend/src/examples/when/when.bn");
        let mut preview = WhenPreview::new(source).expect("when preview");
        assert_eq!(preview.preview_text(), "A: 0B: 0A + BA - B");

        let _ = preview.render_snapshot();
        let tick_port = preview
            .app()
            .event_port_for_source(preview.program.tick_port)
            .expect("tick");
        let addition = preview
            .app()
            .event_port_for_source(preview.program.addition_press_port)
            .expect("addition");
        let subtraction = preview
            .app()
            .event_port_for_source(preview.program.subtraction_press_port)
            .expect("subtraction");

        preview.dispatch_ui_events(tick(tick_port));
        preview.dispatch_ui_events(tick(tick_port));
        preview.dispatch_ui_events(click(addition));
        assert_eq!(preview.preview_text(), "A: 2B: 10A + BA - B12");

        preview.dispatch_ui_events(tick(tick_port));
        preview.dispatch_ui_events(tick(tick_port));
        assert_eq!(preview.preview_text(), "A: 4B: 20A + BA - B12");

        preview.dispatch_ui_events(click(subtraction));
        assert_eq!(preview.preview_text(), "A: 4B: 20A + BA - B-16");
    }

    #[test]
    fn while_preview_recomputes_after_operation_selected() {
        let source = include_str!("../../../playground/frontend/src/examples/while/while.bn");
        let mut preview = WhilePreview::new(source).expect("while preview");
        assert_eq!(preview.preview_text(), "A: 0B: 0A + BA - B");

        let _ = preview.render_snapshot();
        let tick_port = preview
            .app()
            .event_port_for_source(preview.program.tick_port)
            .expect("tick");
        let addition = preview
            .app()
            .event_port_for_source(preview.program.addition_press_port)
            .expect("addition");
        let subtraction = preview
            .app()
            .event_port_for_source(preview.program.subtraction_press_port)
            .expect("subtraction");

        preview.dispatch_ui_events(tick(tick_port));
        preview.dispatch_ui_events(tick(tick_port));
        preview.dispatch_ui_events(click(addition));
        assert_eq!(preview.preview_text(), "A: 2B: 10A + BA - B12");

        preview.dispatch_ui_events(tick(tick_port));
        preview.dispatch_ui_events(tick(tick_port));
        assert_eq!(preview.preview_text(), "A: 4B: 20A + BA - B24");

        preview.dispatch_ui_events(click(subtraction));
        assert_eq!(preview.preview_text(), "A: 4B: 20A + BA - B-16");

        preview.dispatch_ui_events(tick(tick_port));
        preview.dispatch_ui_events(tick(tick_port));
        assert_eq!(preview.preview_text(), "A: 6B: 30A + BA - B-24");
    }
}
