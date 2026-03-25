use crate::interactive_preview::preview_text_from_root;
use crate::ir::MirrorCellId;
use crate::runtime_backed_preview::RuntimeBackedPreviewState;
use boon_renderer_zoon::FakeRenderState;
use boon_scene::{
    RenderOp, RenderRoot, UiEventBatch, UiEventKind, UiFactBatch, UiFactKind, UiNode,
};

pub(crate) trait RuntimeBackedDomain {
    type Action: Clone;
    type FactTarget: Clone;

    fn preview_state(&mut self) -> &mut RuntimeBackedPreviewState<Self::Action, Self::FactTarget>;
    fn render_document(&mut self, ops: &mut Vec<RenderOp>) -> UiNode;
    fn handle_event(
        &mut self,
        action: Self::Action,
        kind: UiEventKind,
        payload: Option<&str>,
    ) -> bool;
    fn handle_fact(&mut self, target: Self::FactTarget, kind: UiFactKind) -> bool;
    fn fact_cell(target: &Self::FactTarget) -> MirrorCellId;
    fn fact_target(cell: MirrorCellId) -> Option<Self::FactTarget>;

    fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool {
        let messages = { self.preview_state().process_ui_events(batch) };
        let mut changed = false;
        for (action, kind, payload) in messages {
            changed |= self.handle_event(action, kind, payload.as_deref());
        }
        changed
    }

    fn dispatch_ui_facts(&mut self, batch: UiFactBatch) -> bool {
        let messages = {
            self.preview_state()
                .process_ui_facts(batch, Self::fact_cell, Self::fact_target)
        };
        let mut changed = false;
        for (target, kind) in messages {
            changed |= self.handle_fact(target, kind);
        }
        changed
    }

    fn render_snapshot(&mut self) -> (RenderRoot, FakeRenderState) {
        self.preview_state().clear_bindings();
        let mut ops = Vec::new();
        let root = self.render_document(&mut ops);
        self.preview_state().finalize_render(root, ops)
    }

    fn preview_text(&mut self) -> String {
        let (root, _) = self.render_snapshot();
        preview_text_from_root(&root)
    }
}
