use crate::host_view_preview::HostViewPreviewApp;
use crate::interactive_preview::{InteractivePreview, render_interactive_preview};
use crate::lower::{
    ButtonHoverTestProgram, ButtonHoverToClickTestProgram, SwitchHoldTestProgram,
    WhileFunctionCallProgram, try_lower_button_hover_test, try_lower_button_hover_to_click_test,
    try_lower_switch_hold_test, try_lower_while_function_call,
};
use boon::platform::browser::kernel::KernelValue;
use boon::zoon::*;
use boon_renderer_zoon::FakeRenderState;
use boon_scene::{NodeId, RenderRoot, UiEventBatch, UiEventKind, UiFactBatch, UiFactKind};
use std::collections::BTreeMap;

pub struct WhileFunctionCallPreview {
    program: WhileFunctionCallProgram,
    show_greeting: bool,
    app: HostViewPreviewApp,
}

impl WhileFunctionCallPreview {
    pub fn new(source: &str) -> Result<Self, String> {
        let program = try_lower_while_function_call(source)?;
        let app = HostViewPreviewApp::new(
            program.host_view.clone(),
            while_function_call_sinks(&program, false),
        );
        Ok(Self {
            program,
            show_greeting: false,
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

    fn sync_sinks(&mut self) {
        for (sink, value) in while_function_call_sinks(&self.program, self.show_greeting) {
            self.app.set_sink_value(sink, value);
        }
    }
}

impl InteractivePreview for WhileFunctionCallPreview {
    fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool {
        let toggle_port = self
            .app
            .event_port_for_source(self.program.toggle_press_port);
        for event in batch.events {
            if event.kind == UiEventKind::Click && Some(event.target) == toggle_port {
                self.show_greeting = !self.show_greeting;
                self.sync_sinks();
                return true;
            }
        }
        false
    }

    fn dispatch_ui_facts(&mut self, _batch: boon_scene::UiFactBatch) -> bool {
        false
    }

    fn render_snapshot(&mut self) -> (RenderRoot, FakeRenderState) {
        let (root, state) = self.app.render_snapshot();
        (RenderRoot::UiTree(root), state)
    }
}

pub fn render_while_function_call_preview(preview: WhileFunctionCallPreview) -> impl Element {
    render_interactive_preview(preview)
}

pub struct ButtonHoverToClickTestPreview {
    program: ButtonHoverToClickTestProgram,
    clicked: [bool; 3],
    app: HostViewPreviewApp,
}

impl ButtonHoverToClickTestPreview {
    pub fn new(source: &str) -> Result<Self, String> {
        let program = try_lower_button_hover_to_click_test(source)?;
        let app = HostViewPreviewApp::new(
            program.host_view.clone(),
            button_hover_to_click_sinks(&program, [false; 3]),
        );
        Ok(Self {
            program,
            clicked: [false; 3],
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

    fn sync_sinks(&mut self) {
        for (sink, value) in button_hover_to_click_sinks(&self.program, self.clicked) {
            self.app.set_sink_value(sink, value);
        }
    }
}

impl InteractivePreview for ButtonHoverToClickTestPreview {
    fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool {
        let button_ports = self
            .program
            .button_press_ports
            .map(|port| self.app.event_port_for_source(port));
        for event in batch.events {
            if event.kind != UiEventKind::Click {
                continue;
            }
            for (index, port) in button_ports.iter().enumerate() {
                if Some(event.target) == *port {
                    self.clicked[index] = !self.clicked[index];
                    self.sync_sinks();
                    return true;
                }
            }
        }
        false
    }

    fn dispatch_ui_facts(&mut self, _batch: boon_scene::UiFactBatch) -> bool {
        false
    }

    fn render_snapshot(&mut self) -> (RenderRoot, FakeRenderState) {
        let (root, state) = self.app.render_snapshot();
        (RenderRoot::UiTree(root), state)
    }
}

pub fn render_button_hover_to_click_test_preview(
    preview: ButtonHoverToClickTestPreview,
) -> impl Element {
    render_interactive_preview(preview)
}

pub struct ButtonHoverTestPreview {
    program: ButtonHoverTestProgram,
    hovered: [bool; 3],
    app: HostViewPreviewApp,
}

impl ButtonHoverTestPreview {
    pub fn new(source: &str) -> Result<Self, String> {
        let program = try_lower_button_hover_test(source)?;
        let app = HostViewPreviewApp::new(
            program.host_view.clone(),
            button_hover_sinks(&program, [false; 3]),
        );
        Ok(Self {
            program,
            hovered: [false; 3],
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

    fn button_ids(&mut self) -> Option<[NodeId; 3]> {
        let root = self.app.render_root();
        let stripe = root.children.first()?;
        let button_row = stripe.children.get(1)?;
        Some([
            button_row.children.first()?.id,
            button_row.children.get(1)?.id,
            button_row.children.get(2)?.id,
        ])
    }

    fn sync_sinks(&mut self) {
        for (sink, value) in button_hover_sinks(&self.program, self.hovered) {
            self.app.set_sink_value(sink, value);
        }
    }
}

impl InteractivePreview for ButtonHoverTestPreview {
    fn dispatch_ui_events(&mut self, _batch: UiEventBatch) -> bool {
        false
    }

    fn dispatch_ui_facts(&mut self, batch: UiFactBatch) -> bool {
        let Some(button_ids) = self.button_ids() else {
            return false;
        };
        let mut changed = false;
        for fact in batch.facts {
            let UiFactKind::Hovered(hovered) = fact.kind else {
                continue;
            };
            for (index, id) in button_ids.iter().enumerate() {
                if fact.id == *id && self.hovered[index] != hovered {
                    self.hovered[index] = hovered;
                    changed = true;
                }
            }
        }
        if changed {
            self.sync_sinks();
        }
        changed
    }

    fn render_snapshot(&mut self) -> (RenderRoot, FakeRenderState) {
        let (root, state) = self.app.render_snapshot();
        (RenderRoot::UiTree(root), state)
    }
}

pub fn render_button_hover_test_preview(preview: ButtonHoverTestPreview) -> impl Element {
    render_interactive_preview(preview)
}

pub struct SwitchHoldTestPreview {
    program: SwitchHoldTestProgram,
    show_item_a: bool,
    click_counts: [u32; 2],
    app: HostViewPreviewApp,
}

impl SwitchHoldTestPreview {
    pub fn new(source: &str) -> Result<Self, String> {
        let program = try_lower_switch_hold_test(source)?;
        let app = HostViewPreviewApp::new(
            program.host_view.clone(),
            switch_hold_test_sinks(&program, true, [0, 0]),
        );
        Ok(Self {
            program,
            show_item_a: true,
            click_counts: [0, 0],
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

    fn sync_sinks(&mut self) {
        for (sink, value) in
            switch_hold_test_sinks(&self.program, self.show_item_a, self.click_counts)
        {
            self.app.set_sink_value(sink, value);
        }
    }
}

impl InteractivePreview for SwitchHoldTestPreview {
    fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool {
        let toggle_port = self
            .app
            .event_port_for_source(self.program.toggle_press_port);
        let item_ports = self
            .program
            .item_press_ports
            .map(|port| self.app.event_port_for_source(port));
        for event in batch.events {
            if event.kind != UiEventKind::Click {
                continue;
            }
            if Some(event.target) == toggle_port {
                self.show_item_a = !self.show_item_a;
                self.sync_sinks();
                return true;
            }
            if Some(event.target) == item_ports[0] && self.show_item_a {
                self.click_counts[0] += 1;
                self.sync_sinks();
                return true;
            }
            if Some(event.target) == item_ports[1] && !self.show_item_a {
                self.click_counts[1] += 1;
                self.sync_sinks();
                return true;
            }
        }
        false
    }

    fn dispatch_ui_facts(&mut self, _batch: boon_scene::UiFactBatch) -> bool {
        false
    }

    fn render_snapshot(&mut self) -> (RenderRoot, FakeRenderState) {
        let (root, state) = self.app.render_snapshot();
        (RenderRoot::UiTree(root), state)
    }
}

pub fn render_switch_hold_test_preview(preview: SwitchHoldTestPreview) -> impl Element {
    render_interactive_preview(preview)
}

fn bool_text(value: bool) -> &'static str {
    if value { "True" } else { "False" }
}

fn while_function_call_sinks(
    program: &WhileFunctionCallProgram,
    show_greeting: bool,
) -> BTreeMap<crate::ir::SinkPortId, KernelValue> {
    BTreeMap::from([
        (
            program.toggle_label_sink,
            KernelValue::from(format!("Toggle (show: {})", bool_text(show_greeting))),
        ),
        (
            program.content_sink,
            KernelValue::from(if show_greeting {
                "Hello, World!"
            } else {
                "Hidden"
            }),
        ),
    ])
}

fn button_hover_to_click_sinks(
    program: &ButtonHoverToClickTestProgram,
    clicked: [bool; 3],
) -> BTreeMap<crate::ir::SinkPortId, KernelValue> {
    BTreeMap::from([
        (
            program.intro_sink,
            KernelValue::from("Click each button - clicked ones turn darker with outline"),
        ),
        (
            program.button_active_sinks[0],
            KernelValue::Bool(clicked[0]),
        ),
        (
            program.button_active_sinks[1],
            KernelValue::Bool(clicked[1]),
        ),
        (
            program.button_active_sinks[2],
            KernelValue::Bool(clicked[2]),
        ),
        (
            program.state_sink,
            KernelValue::from(format!(
                "States - A: {}, B: {}, C: {}",
                bool_text(clicked[0]),
                bool_text(clicked[1]),
                bool_text(clicked[2])
            )),
        ),
    ])
}

fn button_hover_sinks(
    program: &ButtonHoverTestProgram,
    hovered: [bool; 3],
) -> BTreeMap<crate::ir::SinkPortId, KernelValue> {
    BTreeMap::from([
        (
            program.intro_sink,
            KernelValue::from("Hover each button - only hovered one should show border"),
        ),
        (program.button_hover_sinks[0], KernelValue::Bool(hovered[0])),
        (program.button_hover_sinks[1], KernelValue::Bool(hovered[1])),
        (program.button_hover_sinks[2], KernelValue::Bool(hovered[2])),
    ])
}

fn switch_hold_test_sinks(
    program: &SwitchHoldTestProgram,
    show_item_a: bool,
    click_counts: [u32; 2],
) -> BTreeMap<crate::ir::SinkPortId, KernelValue> {
    let (item_name, count, disabled) = if show_item_a {
        ("Item A", click_counts[0], [false, true])
    } else {
        ("Item B", click_counts[1], [true, false])
    };
    BTreeMap::from([
        (
            program.current_item_sink,
            KernelValue::from(format!("Showing: {item_name}")),
        ),
        (
            program.current_count_sink,
            KernelValue::from(format!("{item_name} clicks: {count}")),
        ),
        (
            program.item_disabled_sinks[0],
            KernelValue::Bool(disabled[0]),
        ),
        (
            program.item_disabled_sinks[1],
            KernelValue::Bool(disabled[1]),
        ),
        (
            program.footer_sink,
            KernelValue::from(
                "Test: Click button, toggle view, click again. Counts should increment correctly.",
            ),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use boon_scene::{UiEvent, UiEventKind};

    #[test]
    fn while_function_call_preview_toggles_greeting_branch() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/while_function_call/while_function_call.bn"
        );
        let mut preview =
            WhileFunctionCallPreview::new(source).expect("while_function_call preview");
        assert_eq!(preview.preview_text(), "Toggle (show: False)Hidden");

        let _ = preview.render_snapshot();
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(preview.program.toggle_press_port)
                    .expect("toggle port"),
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        assert_eq!(preview.preview_text(), "Toggle (show: True)Hello, World!");
    }

    #[test]
    fn button_hover_to_click_preview_tracks_independent_button_state() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/button_hover_to_click_test/button_hover_to_click_test.bn"
        );
        let mut preview =
            ButtonHoverToClickTestPreview::new(source).expect("button_hover_to_click_test preview");
        assert_eq!(
            preview.preview_text(),
            "Click each button - clicked ones turn darker with outlineButton AButton BButton CStates - A: False, B: False, C: False"
        );

        let _ = preview.render_snapshot();
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(preview.program.button_press_ports[0])
                    .expect("button A port"),
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(preview.program.button_press_ports[2])
                    .expect("button C port"),
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        assert_eq!(
            preview.preview_text(),
            "Click each button - clicked ones turn darker with outlineButton AButton BButton CStates - A: True, B: False, C: True"
        );
    }

    #[test]
    fn button_hover_test_preview_tracks_independent_hover_state() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/button_hover_test/button_hover_test.bn"
        );
        let mut preview = ButtonHoverTestPreview::new(source).expect("button_hover_test preview");
        assert_eq!(
            preview.preview_text(),
            "Hover each button - only hovered one should show borderButton AButton BButton C"
        );

        let button_ids = preview.button_ids().expect("button ids");
        preview.dispatch_ui_facts(UiFactBatch {
            facts: vec![boon_scene::UiFact {
                id: button_ids[1],
                kind: UiFactKind::Hovered(true),
            }],
        });
        let (root, state) = preview.render_snapshot();
        let RenderRoot::UiTree(root) = root else {
            panic!("expected ui tree render root");
        };
        let button_row = &root.children[0].children[1];
        assert_eq!(
            state.style_value(button_row.children[0].id, "outline"),
            Some("none")
        );
        assert_eq!(
            state.style_value(button_row.children[1].id, "outline"),
            Some("2px solid oklch(0.6 0.2 250)")
        );
        assert_eq!(
            state.style_value(button_row.children[2].id, "outline"),
            Some("none")
        );
    }

    #[test]
    fn switch_hold_test_preview_keeps_counts_across_view_switches() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/switch_hold_test/switch_hold_test.bn"
        );
        let mut preview = SwitchHoldTestPreview::new(source).expect("switch_hold_test preview");
        assert_eq!(
            preview.preview_text(),
            "Showing: Item AToggle ViewItem A clicks: 0Click Item AClick Item BTest: Click button, toggle view, click again. Counts should increment correctly."
        );

        let _ = preview.render_snapshot();
        let item_a_port = preview
            .app()
            .event_port_for_source(preview.program.item_press_ports[0])
            .expect("item A port");
        let item_b_port = preview
            .app()
            .event_port_for_source(preview.program.item_press_ports[1])
            .expect("item B port");
        let toggle_port = preview
            .app()
            .event_port_for_source(preview.program.toggle_press_port)
            .expect("toggle port");

        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: item_a_port,
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: toggle_port,
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: item_b_port,
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        assert_eq!(
            preview.preview_text(),
            "Showing: Item BToggle ViewItem B clicks: 1Click Item AClick Item BTest: Click button, toggle view, click again. Counts should increment correctly."
        );
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: toggle_port,
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        assert_eq!(
            preview.preview_text(),
            "Showing: Item AToggle ViewItem A clicks: 1Click Item AClick Item BTest: Click button, toggle view, click again. Counts should increment correctly."
        );
    }
}
