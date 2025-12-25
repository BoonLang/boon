//! Interpreter for the new arena-based engine (v2).
//!
//! This module connects the new engine_v2 to the playground UI.

use crate::engine_v2::{
    arena::SlotId,
    event_loop::EventLoop,
};
use crate::evaluator_v2::CompileContext;
use crate::parser::{
    Input, Spanned, Token, lexer, parser, reset_expression_depth, span_at,
};
use crate::platform::browser::bridge_v2::{BridgeContext, ReactiveEventLoop};
use chumsky::prelude::Parser as ChumskyParser;
use zoon::*;

/// Result of running the v2 interpreter.
pub struct InterpreterResult {
    /// The compiled event loop
    pub event_loop: EventLoop,
    /// The root slot (document or last expression)
    pub root_slot: Option<SlotId>,
}

/// Run Boon code using the new arena-based engine.
///
/// Returns the event loop and root slot for bridge rendering.
pub fn run_v2(source_code: &str) -> Option<InterpreterResult> {
    reset_expression_depth();

    // Lex the code
    let (tokens, lex_errors) = lexer().parse(source_code).into_output_errors();

    if !lex_errors.is_empty() {
        zoon::println!("Lex errors: {:?}", lex_errors);
        return None;
    }

    let mut tokens = tokens?;

    // Filter comments
    tokens.retain(|t| !matches!(t.node, Token::Comment(_)));

    // Create input with span mapping
    let input = tokens.map(
        span_at(source_code.len()),
        |Spanned { node, span, persistence: _ }| (node, span),
    );

    // Parse
    let (expressions, parse_errors) = parser().parse(input).into_output_errors();

    if !parse_errors.is_empty() {
        zoon::println!("Parse errors: {:?}", parse_errors);
        return None;
    }

    let expressions = expressions?;

    // Create event loop and compile context
    let mut event_loop = EventLoop::new();
    let mut ctx = CompileContext::new(&mut event_loop);

    // Compile the program
    let root_slot = ctx.compile_program(&expressions);

    // Mark all nodes as dirty to trigger initial evaluation
    let all_slots: Vec<_> = (0..event_loop.arena_len() as u32)
        .filter_map(|idx| {
            let slot = SlotId { index: idx, generation: 0 };
            if event_loop.is_valid(slot) { Some(slot) } else { None }
        })
        .collect();

    for slot in all_slots {
        event_loop.mark_dirty(slot, crate::engine_v2::address::Port::Output);
    }

    // Run until quiescent (max 1000 ticks for safety)
    // Note: We ignore timer_queue - real timers are handled asynchronously via pending_real_timers
    for _ in 0..1000 {
        event_loop.run_tick();
        if event_loop.dirty_nodes.is_empty() {
            break;
        }
    }

    Some(InterpreterResult {
        event_loop,
        root_slot,
    })
}

/// Get the display string for the root value.
pub fn get_display_string(result: &InterpreterResult) -> String {
    let ctx = BridgeContext::new(&result.event_loop);

    if let Some(slot) = result.root_slot {
        if let Some(payload) = ctx.get_slot_value(slot) {
            return ctx.render_scalar(payload);
        }
    }

    String::new()
}

/// Run Boon code and return a Zoon element for the playground.
///
/// This is the entry point for the playground UI with engine-v2.
/// Takes ownership of the source code to avoid lifetime issues.
pub fn run_and_render(source_code: String) -> impl Element + use<> {
    let result = run_v2(&source_code);

    match result {
        Some(r) => {
            // Check if this is a simple scalar result or a UI with elements
            let ctx = BridgeContext::new(&r.event_loop);
            let is_scalar = r.root_slot
                .and_then(|slot| ctx.get_slot_value(slot))
                .map(|p| ctx.is_scalar(p))
                .unwrap_or(true);

            if is_scalar {
                // Static scalar rendering
                let display = r.root_slot
                    .and_then(|slot| ctx.get_slot_value(slot))
                    .map(|p| ctx.render_scalar(p))
                    .unwrap_or_default();

                return El::new()
                    .s(Width::fill())
                    .s(Height::fill())
                    .s(Padding::all(16))
                    .s(Font::new()
                        .size(16)
                        .family([FontFamily::new("Source Code Pro"), FontFamily::Monospace])
                        .color(hsluv!(0, 0, 100)))
                    .child(display)
                    .unify();
            }

            // Interactive UI rendering with reactive event loop
            let reactive_el = ReactiveEventLoop::new(r.event_loop, r.root_slot);

            // Create a signal that re-renders when version changes
            let reactive_el_clone = reactive_el.clone();
            let element_signal = reactive_el.version.signal()
                .map(move |_version| {
                    reactive_el_clone.render_element()
                });

            El::new()
                .s(Width::fill())
                .s(Height::fill())
                .child_signal(element_signal)
                .unify()
        }
        None => {
            // Error
            El::new()
                .s(Width::fill())
                .s(Height::fill())
                .s(Padding::all(16))
                .s(Font::new()
                    .size(16)
                    .family([FontFamily::new("Source Code Pro"), FontFamily::Monospace])
                    .color(hsluv!(0, 70, 60)))
                .child("Failed to run code (v2 engine). Check console for errors.")
                .unify()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_simple_expression() {
        let result = run_v2("42").unwrap();
        assert!(result.root_slot.is_some());
        let display = get_display_string(&result);
        assert_eq!(display, "42");
    }

    #[test]
    fn run_arithmetic() {
        let result = run_v2("1 + 2").unwrap();
        let display = get_display_string(&result);
        assert_eq!(display, "3");
    }

    #[test]
    fn run_variable() {
        let result = run_v2("x: 42").unwrap();
        let display = get_display_string(&result);
        assert_eq!(display, "42");
    }

    #[test]
    fn run_latest() {
        let result = run_v2("LATEST { 1, 2, 3 }").unwrap();
        let display = get_display_string(&result);
        assert_eq!(display, "3");
    }
}
