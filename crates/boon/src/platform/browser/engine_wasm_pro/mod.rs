//! Wasm Pro backend scaffold.
//!
//! This module is the parallel entry point for the next-generation Wasm backend.
//! The implementation here is intentionally minimal during milestone 0 so the
//! engine can be selected explicitly without falling back to the legacy Wasm
//! engine or other backends.

pub mod abi;
mod codegen;
mod debug;
mod exec_ir;
mod lower;
mod runtime;
mod semantic_ir;

use boon_renderer_zoon::{FakeRenderState, missing_document_root};
use boon_scene::{UiEventBatch, UiFactBatch};
use std::cell::RefCell;
use std::rc::Rc;
use zoon::*;

pub type ExternalFunction = (
    String,
    Vec<String>,
    crate::parser::static_expression::Spanned<crate::parser::static_expression::Expression>,
    Option<String>,
);

pub fn run_wasm_pro(
    source: &str,
    external_functions: Option<&[ExternalFunction]>,
    persistence_enabled: bool,
) -> RawElOrText {
    let semantic = lower::lower_to_semantic(source, external_functions, persistence_enabled);
    let exec = exec_ir::ExecProgram::from_semantic(&semantic);
    let _summary = debug::summarize(&semantic, &exec);
    let runtime = Rc::new(RefCell::new(runtime::bootstrap_runtime(
        source,
        external_functions.map_or(0, |functions| functions.len()),
        persistence_enabled,
    )));
    let render_state = Rc::new(RefCell::new(FakeRenderState::default()));
    let version = Mutable::new(0_u64);

    let _init_descriptor = runtime.borrow_mut().init(&exec);
    let descriptor = runtime.borrow_mut().take_commands();
    let Ok(batch) = runtime.borrow().decode_commands(descriptor) else {
        return missing_document_root();
    };
    if render_state.borrow_mut().apply_batch(&batch).is_err() {
        return missing_document_root();
    }

    let apply_descriptor: Rc<dyn Fn(u64)> = Rc::new({
        let runtime = runtime.clone();
        let render_state = render_state.clone();
        let version = version.clone();
        move |descriptor: u64| {
            if descriptor == 0 {
                return;
            }
            let Ok(batch) = runtime.borrow().decode_commands(descriptor) else {
                return;
            };
            if render_state.borrow_mut().apply_batch(&batch).is_err() {
                return;
            }
            version.update(|current| current + 1);
        }
    });

    let event_handler = {
        let runtime = runtime.clone();
        let apply_descriptor = apply_descriptor.clone();
        move |batch: UiEventBatch| {
            let bytes = abi::encode_ui_event_batch(&batch);
            let Ok(_) = runtime.borrow_mut().dispatch_events(&bytes) else {
                return;
            };
            let descriptor = runtime.borrow_mut().take_commands();
            apply_descriptor(descriptor);
        }
    };

    let fact_handler = {
        let runtime = runtime.clone();
        let apply_descriptor = apply_descriptor.clone();
        move |batch: UiFactBatch| {
            let bytes = abi::encode_ui_fact_batch(&batch);
            let Ok(_) = runtime.borrow_mut().apply_facts(&bytes) else {
                return;
            };
            let descriptor = runtime.borrow_mut().take_commands();
            apply_descriptor(descriptor);
        }
    };

    let handlers = boon_renderer_zoon::RenderInteractionHandlers::new(event_handler, fact_handler);
    El::new()
        .child_signal(version.signal().map({
            let render_state = render_state.clone();
            let handlers = handlers.clone();
            move |_| {
                Some(boon_renderer_zoon::render_fake_state_with_handlers(
                    &render_state.borrow(),
                    &handlers,
                ))
            }
        }))
        .unify()
}

pub fn clear_wasm_pro_persisted_states() {}
