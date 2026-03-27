use boon::zoon::*;
use boon_renderer_zoon::{FakeRenderState, RenderInteractionHandlers, render_retained_snapshot_signal};
use boon_scene::{RenderRoot, UiEventBatch, UiNode};
use std::cell::RefCell;
use std::rc::Rc;

pub fn render_preview_shell<S: 'static>(
    state: S,
    on_events: impl Fn(&mut S, UiEventBatch) + 'static,
    render_snapshot: impl Fn(&mut S) -> (UiNode, FakeRenderState) + 'static,
) -> impl Element {
    let state = Rc::new(RefCell::new(state));
    let snapshot = Mutable::new({
        let (root, render_state) = render_snapshot(&mut state.borrow_mut());
        (RenderRoot::UiTree(root), render_state)
    });

    let handlers = RenderInteractionHandlers::new(
        {
            let state = state.clone();
            let snapshot = snapshot.clone();
            move |batch: UiEventBatch| {
                on_events(&mut state.borrow_mut(), batch);
                let (root, render_state) = render_snapshot(&mut state.borrow_mut());
                snapshot.set((RenderRoot::UiTree(root), render_state));
            }
        },
        |_facts| {},
    );

    render_retained_snapshot_signal(snapshot.signal_cloned(), handlers)
}
