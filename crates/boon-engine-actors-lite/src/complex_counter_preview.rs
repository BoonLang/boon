use crate::bridge::{HostInput, HostSnapshot};
use crate::host_view_preview::HostViewPreviewApp;
use crate::ids::ActorId;
use crate::interactive_preview::{InteractivePreview, render_interactive_preview};
use crate::ir_executor::IrExecutor;
use crate::lower::{ComplexCounterProgram, try_lower_complex_counter};
use crate::preview_runtime::PreviewRuntime;
use crate::runtime::ActorKind;
use boon::platform::browser::kernel::KernelValue;
use boon::zoon::*;
use boon_renderer_zoon::FakeRenderState;
use boon_scene::{NodeId, RenderRoot, UiEventBatch, UiEventKind, UiFactBatch, UiFactKind, UiNode};

pub struct ComplexCounterPreview {
    runtime: PreviewRuntime,
    decrement_actor: ActorId,
    increment_actor: ActorId,
    program: ComplexCounterProgram,
    executor: IrExecutor,
    app: HostViewPreviewApp,
}

impl ComplexCounterPreview {
    pub fn new(source: &str) -> Result<Self, String> {
        let program = try_lower_complex_counter(source)?;
        let mut runtime = PreviewRuntime::new();
        let decrement_actor = runtime.alloc_actor(ActorKind::SourcePort);
        let increment_actor = runtime.alloc_actor(ActorKind::SourcePort);
        let executor = IrExecutor::new(program.ir.clone())?;

        let sink_values = executor.sink_values().clone();
        let app = HostViewPreviewApp::new(program.host_view.clone(), sink_values);

        Ok(Self {
            runtime,
            decrement_actor,
            increment_actor,
            program,
            executor,
            app,
        })
    }

    pub fn click_decrement(&mut self) {
        let messages = self.runtime.dispatch_pulse(
            self.decrement_actor,
            self.program.decrement_port,
            KernelValue::from("press"),
        );
        self.apply_runtime_messages(messages);
    }

    pub fn click_increment(&mut self) {
        let messages = self.runtime.dispatch_pulse(
            self.increment_actor,
            self.program.increment_port,
            KernelValue::from("press"),
        );
        self.apply_runtime_messages(messages);
    }

    pub fn dispatch_ui_events(&mut self, batch: UiEventBatch) {
        let _ = self.render_root();
        let decrement_port = self.app.event_port_for_source(self.program.decrement_port);
        let increment_port = self.app.event_port_for_source(self.program.increment_port);
        for event in batch.events {
            if event.kind != UiEventKind::Click {
                continue;
            }
            if Some(event.target) == decrement_port {
                self.click_decrement();
            } else if Some(event.target) == increment_port {
                self.click_increment();
            }
        }
    }

    pub fn dispatch_ui_facts(&mut self, batch: UiFactBatch) {
        let Some((decrement_id, increment_id)) = self.button_ids() else {
            return;
        };
        let mut inputs = Vec::new();
        for fact in batch.facts {
            let UiFactKind::Hovered(hovered) = fact.kind else {
                continue;
            };
            let (actor, cell) = if fact.id == decrement_id {
                (self.decrement_actor, self.program.decrement_hovered_cell)
            } else if fact.id == increment_id {
                (self.increment_actor, self.program.increment_hovered_cell)
            } else {
                continue;
            };
            inputs.push(HostInput::Mirror {
                actor,
                cell,
                value: KernelValue::Bool(hovered),
                seq: self.runtime.causal_seq(inputs.len() as u32),
            });
        }
        if inputs.is_empty() {
            return;
        }
        let messages = self.runtime.dispatch_snapshot(HostSnapshot::new(inputs));
        self.apply_runtime_messages(messages);
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

    fn button_ids(&mut self) -> Option<(NodeId, NodeId)> {
        let root = self.render_root();
        let stripe = root.children.first()?;
        let decrement = stripe.children.first()?;
        let increment = stripe.children.get(2)?;
        Some((decrement.id, increment.id))
    }

    fn apply_runtime_messages(&mut self, messages: Vec<(ActorId, crate::runtime::Msg)>) {
        self.executor
            .apply_messages(&messages)
            .expect("complex counter IR should execute");
        for (sink, value) in self.executor.sink_values() {
            self.app.set_sink_value(*sink, value.clone());
        }
    }
}

impl InteractivePreview for ComplexCounterPreview {
    fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool {
        let before = self.preview_text();
        ComplexCounterPreview::dispatch_ui_events(self, batch);
        self.preview_text() != before
    }

    fn dispatch_ui_facts(&mut self, batch: UiFactBatch) -> bool {
        let before = self.preview_text();
        ComplexCounterPreview::dispatch_ui_facts(self, batch);
        self.preview_text() != before
    }

    fn render_snapshot(&mut self) -> (RenderRoot, FakeRenderState) {
        let (root, state) = ComplexCounterPreview::render_snapshot(self);
        (RenderRoot::UiTree(root), state)
    }
}

pub fn render_complex_counter_preview(preview: ComplexCounterPreview) -> impl Element {
    render_interactive_preview(preview)
}

#[cfg(test)]
mod tests {
    use super::*;
    use boon_scene::{UiFact, UiFactBatch, UiFactKind};

    #[test]
    fn complex_counter_preview_updates_both_directions() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/complex_counter/complex_counter.bn"
        );
        let mut preview = ComplexCounterPreview::new(source).expect("complex counter preview");
        assert_eq!(preview.preview_text(), "-0+");
        preview.click_increment();
        preview.click_increment();
        assert_eq!(preview.preview_text(), "-2+");
        preview.click_decrement();
        assert_eq!(preview.preview_text(), "-1+");
    }

    #[test]
    fn complex_counter_preview_hover_updates_button_background() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/complex_counter/complex_counter.bn"
        );
        let mut preview = ComplexCounterPreview::new(source).expect("complex counter preview");
        let (decrement_id, increment_id) = preview.button_ids().expect("counter buttons");

        preview.dispatch_ui_facts(UiFactBatch {
            facts: vec![UiFact {
                id: decrement_id,
                kind: UiFactKind::Hovered(true),
            }],
        });
        let (root, state) = preview.render_snapshot();
        let decrement = &root.children[0].children[0];
        let increment = &root.children[0].children[2];
        assert_eq!(
            state.style_value(decrement.id, "background"),
            Some("oklch(0.85 0.07 320)")
        );
        assert_eq!(
            state.style_value(increment.id, "background"),
            Some("oklch(0.75 0.07 320)")
        );

        preview.dispatch_ui_facts(UiFactBatch {
            facts: vec![
                UiFact {
                    id: decrement_id,
                    kind: UiFactKind::Hovered(false),
                },
                UiFact {
                    id: increment_id,
                    kind: UiFactKind::Hovered(true),
                },
            ],
        });
        let (root, state) = preview.render_snapshot();
        let decrement = &root.children[0].children[0];
        let increment = &root.children[0].children[2];
        assert_eq!(
            state.style_value(decrement.id, "background"),
            Some("oklch(0.75 0.07 320)")
        );
        assert_eq!(
            state.style_value(increment.id, "background"),
            Some("oklch(0.85 0.07 320)")
        );
    }
}
