use boon::zoon::*;
use boon_renderer_zoon::{
    FakeRenderState, RenderInteractionHandlers, render_snapshot_root_with_handlers,
};
use boon_scene::{RenderRoot, UiEventBatch, UiNode};
use std::cell::RefCell;
use std::rc::Rc;

pub fn render_preview_shell<S: 'static>(
    state: S,
    on_events: impl Fn(&mut S, UiEventBatch) + 'static,
    render_snapshot: impl Fn(&mut S) -> (UiNode, FakeRenderState) + 'static,
) -> impl Element {
    let state = Rc::new(RefCell::new(state));
    let version = Mutable::new(0u64);

    let handlers = RenderInteractionHandlers::new(
        {
            let state = state.clone();
            let version = version.clone();
            move |batch: UiEventBatch| {
                on_events(&mut state.borrow_mut(), batch);
                version.update(|value| value + 1);
            }
        },
        |_facts| {},
    );

    El::new().child_signal(version.signal().map({
        let state = state.clone();
        let handlers = handlers.clone();
        move |_| {
            let (root, render_state) = render_snapshot(&mut state.borrow_mut());
            let root = RenderRoot::UiTree(root);
            Some(render_snapshot_root_with_handlers(
                &root,
                &render_state,
                &handlers,
            ))
        }
    }))
}
