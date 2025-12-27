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
use crate::platform::browser::bridge_v2::{BridgeContext, ReactiveEventLoop, invalidate_all_timers};
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
    run_v2_with_storage(source_code, None)
}

/// Run Boon code with optional storage key for persistence.
pub fn run_v2_with_storage(source_code: &str, storage_key: Option<&str>) -> Option<InterpreterResult> {
    // ===== PERFORMANCE TIMING INFRASTRUCTURE =====
    // Uncomment the timing lines below to diagnose performance issues.
    // Each phase is measured separately to identify bottlenecks.

    #[cfg(target_arch = "wasm32")]
    let total_start = web_sys::window().and_then(|w| w.performance()).map(|p| p.now());

    reset_expression_depth();

    // ----- PHASE 1: LEX -----
    #[cfg(target_arch = "wasm32")]
    let lex_start = web_sys::window().and_then(|w| w.performance()).map(|p| p.now());

    let (tokens, lex_errors) = lexer().parse(source_code).into_output_errors();

    #[cfg(target_arch = "wasm32")]
    if let (Some(start), Some(perf)) = (lex_start, web_sys::window().and_then(|w| w.performance())) {
        zoon::println!("[PERF] Lex: {:.2}ms ({} chars)", perf.now() - start, source_code.len());
    }

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

    // ----- PHASE 2: PARSE -----
    #[cfg(target_arch = "wasm32")]
    let parse_start = web_sys::window().and_then(|w| w.performance()).map(|p| p.now());

    let (expressions, parse_errors) = parser().parse(input).into_output_errors();

    #[cfg(target_arch = "wasm32")]
    if let (Some(start), Some(perf)) = (parse_start, web_sys::window().and_then(|w| w.performance())) {
        zoon::println!("[PERF] Parse: {:.2}ms", perf.now() - start);
    }

    if !parse_errors.is_empty() {
        zoon::println!("Parse errors: {:?}", parse_errors);
        return None;
    }

    let expressions = expressions?;

    // ----- PHASE 3: COMPILE -----
    #[cfg(target_arch = "wasm32")]
    let compile_start = web_sys::window().and_then(|w| w.performance()).map(|p| p.now());

    let mut event_loop = EventLoop::new();
    let mut ctx = CompileContext::new(&mut event_loop);
    let root_slot = ctx.compile_program(&expressions);

    #[cfg(target_arch = "wasm32")]
    if let (Some(start), Some(perf)) = (compile_start, web_sys::window().and_then(|w| w.performance())) {
        zoon::println!("[PERF] Compile: {:.2}ms ({} nodes)", perf.now() - start, event_loop.arena_len());
    }

    // Load persistence BEFORE initial ticks (so persisted values aren't overwritten)
    #[cfg(target_arch = "wasm32")]
    if let Some(key) = storage_key {
        load_persistence_into_event_loop(&mut event_loop, key);
    }

    // ----- PHASE 4: INITIAL TICKS -----
    #[cfg(target_arch = "wasm32")]
    let tick_start = web_sys::window().and_then(|w| w.performance()).map(|p| p.now());

    // Mark all nodes as dirty to trigger initial evaluation
    let all_slots: Vec<_> = (0..event_loop.arena_len() as u32)
        .filter_map(|idx| {
            let slot = SlotId { index: idx, generation: 0 };
            if event_loop.is_valid(slot) { Some(slot) } else { None }
        })
        .collect();

    #[cfg(target_arch = "wasm32")]
    let num_slots = all_slots.len();

    for slot in all_slots {
        event_loop.mark_dirty(slot, crate::engine_v2::address::Port::Output);
    }

    // Run until quiescent (max 1000 ticks for safety)
    #[cfg(target_arch = "wasm32")]
    let mut tick_count = 0u32;

    for _ in 0..1000 {
        event_loop.run_tick();
        #[cfg(target_arch = "wasm32")]
        { tick_count += 1; }
        if event_loop.dirty_nodes.is_empty() {
            break;
        }
    }

    #[cfg(target_arch = "wasm32")]
    if let (Some(start), Some(perf)) = (tick_start, web_sys::window().and_then(|w| w.performance())) {
        zoon::println!("[PERF] Initial ticks: {:.2}ms ({} ticks, {} slots marked dirty)",
            perf.now() - start, tick_count, num_slots);
    }

    // ----- TOTAL TIME -----
    #[cfg(target_arch = "wasm32")]
    if let (Some(start), Some(perf)) = (total_start, web_sys::window().and_then(|w| w.performance())) {
        zoon::println!("[PERF] TOTAL run_v2: {:.2}ms", perf.now() - start);
    }

    Some(InterpreterResult {
        event_loop,
        root_slot,
    })
}

/// Load persistence state into the event loop before initial ticks.
#[cfg(target_arch = "wasm32")]
fn load_persistence_into_event_loop(event_loop: &mut EventLoop, storage_key: &str) {
    use std::collections::BTreeMap;
    use crate::engine_v2::node::NodeKind;
    use crate::engine_v2::message::Payload;

    let states: BTreeMap<String, serde_json::Value> = match local_storage().get(storage_key) {
        None => return,
        Some(Ok(states)) => states,
        Some(Err(error)) => {
            zoon::eprintln!("Failed to load state: {error:#}");
            return;
        }
    };

    if states.is_empty() {
        return;
    }

    zoon::println!("Loading {} states from localStorage (pre-tick)", states.len());

    for (key, json) in states {
        if let Some(index_str) = key.strip_prefix("slot:") {
            if let Ok(index) = index_str.parse::<u32>() {
                let slot = SlotId { index, generation: 0 };

                if !event_loop.arena.is_valid(slot) {
                    continue;
                }

                if let Some(node) = event_loop.arena.get_mut(slot) {
                    match node.kind_mut() {
                        Some(NodeKind::Register { stored_value, .. }) => {
                            if let Some(payload) = json_to_payload(&json) {
                                zoon::println!("  Pre-tick restore Register slot {} with {:?}", index, payload);
                                *stored_value = Some(payload.clone());
                                node.extension_mut().current_value = Some(payload);
                            }
                        }
                        Some(NodeKind::Accumulator { sum, has_input }) => {
                            if let Some(obj) = json.as_object() {
                                if obj.get("type").and_then(|v| v.as_str()) == Some("Accumulator") {
                                    if let Some(s) = obj.get("sum").and_then(|v| v.as_f64()) {
                                        zoon::println!("  Pre-tick restore Accumulator slot {} with sum {}", index, s);
                                        *sum = s;
                                        *has_input = true; // Restored value counts as having received input
                                        node.extension_mut().current_value = Some(Payload::Number(s));
                                    }
                                }
                            }
                        }
                        _ => {
                            // Check if this is a Bus that needs restoring
                            // (handled separately below since we need mutable arena access)
                        }
                    }
                }
            }
        }
    }

    // Second pass: Restore Bus (LIST) items
    // Done separately because we need to allocate new slots for items
    let states: BTreeMap<String, serde_json::Value> = match local_storage().get(storage_key) {
        None => return,
        Some(Ok(states)) => states,
        Some(Err(_)) => return,
    };

    for (key, json) in states {
        if let Some(index_str) = key.strip_prefix("slot:") {
            if let Ok(index) = index_str.parse::<u32>() {
                let slot = SlotId { index, generation: 0 };

                // Check if this is a Bus snapshot
                if let Some(obj) = json.as_object() {
                    if obj.get("type").and_then(|v| v.as_str()) == Some("Bus") {
                        if !event_loop.arena.is_valid(slot) {
                            continue;
                        }

                        // Get the saved data
                        let next_instance = obj.get("next_instance")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        let saved_items = obj.get("items")
                            .and_then(|v| v.as_array())
                            .cloned()
                            .unwrap_or_default();

                        if saved_items.is_empty() {
                            continue;
                        }

                        zoon::println!("  Pre-tick restore Bus slot {} with {} items, next_instance={}",
                            index, saved_items.len(), next_instance);

                        // Create new slots for each saved item and collect them
                        let mut new_items: Vec<(u64, SlotId)> = Vec::new();

                        for item_json in saved_items {
                            let item_obj = match item_json.as_object() {
                                Some(obj) => obj,
                                None => continue,
                            };

                            let item_key = match item_obj.get("key").and_then(|v| v.as_u64()) {
                                Some(k) => k,
                                None => continue,
                            };

                            let value_json = match item_obj.get("value") {
                                Some(v) => v,
                                None => continue,
                            };

                            // Try to restore the item - either as simple payload or object structure
                            let item_slot = if let Some(payload) = json_to_payload(value_json) {
                                // Simple scalar value - create a Producer
                                let item_slot = event_loop.arena.alloc();
                                if let Some(item_node) = event_loop.arena.get_mut(item_slot) {
                                    item_node.set_kind(NodeKind::Producer {
                                        value: Some(payload.clone()),
                                    });
                                    item_node.extension_mut().current_value = Some(payload);
                                }
                                zoon::println!("    Restored scalar item key={} at slot {:?}", item_key, item_slot);
                                Some(item_slot)
                            } else {
                                // Try to restore as Object structure (Router with fields)
                                let result = restore_object_structure(event_loop, value_json);
                                if result.is_some() {
                                    zoon::println!("    Restored object item key={} at slot {:?}", item_key, result);
                                }
                                result
                            };

                            if let Some(slot) = item_slot {
                                new_items.push((item_key, slot));
                            }
                        }

                        // Now update the Bus with restored items
                        if let Some(bus_node) = event_loop.arena.get_mut(slot) {
                            if let Some(NodeKind::Bus { items, alloc_site, .. }) = bus_node.kind_mut() {
                                // Restore the AllocSite counter
                                alloc_site.next_instance = next_instance;

                                // Add the restored items
                                for (item_key, item_slot) in new_items {
                                    items.push((item_key, item_slot));
                                }

                                zoon::println!("    Bus now has {} items, next_instance={}",
                                    items.len(), alloc_site.next_instance);
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Convert JSON to Payload for persistence loading.
#[cfg(target_arch = "wasm32")]
fn json_to_payload(json: &serde_json::Value) -> Option<crate::engine_v2::message::Payload> {
    use crate::engine_v2::message::Payload;

    let obj = json.as_object()?;
    let type_str = obj.get("type")?.as_str()?;

    Some(match type_str {
        "Number" => Payload::Number(obj.get("value")?.as_f64()?),
        "Text" => Payload::Text(obj.get("value")?.as_str()?.into()),
        "Bool" => Payload::Bool(obj.get("value")?.as_bool()?),
        "Unit" => Payload::Unit,
        "Tag" => Payload::Tag(obj.get("value")?.as_u64()? as u32),
        _ => return None,
    })
}

/// Restore an Object structure from JSON, creating Router and Producer nodes.
/// Returns the root SlotId of the restored structure.
#[cfg(target_arch = "wasm32")]
fn restore_object_structure(
    event_loop: &mut EventLoop,
    json: &serde_json::Value,
) -> Option<SlotId> {
    use crate::engine_v2::message::Payload;
    use crate::engine_v2::node::NodeKind;
    use std::collections::HashMap;

    let obj = json.as_object()?;
    let type_str = obj.get("type")?.as_str()?;

    match type_str {
        "Object" => {
            // Restore a Router with fields
            let fields_json = obj.get("fields")?.as_object()?;
            let mut fields: HashMap<crate::engine_v2::message::FieldId, SlotId> = HashMap::new();

            for (name, value) in fields_json {
                // Recursively restore each field
                if let Some(field_slot) = restore_object_structure(event_loop, value) {
                    let field_id = event_loop.arena.intern_field(name);
                    fields.insert(field_id, field_slot);
                }
            }

            if fields.is_empty() {
                return None;
            }

            // Create Router node
            let router_slot = event_loop.arena.alloc();
            if let Some(node) = event_loop.arena.get_mut(router_slot) {
                node.set_kind(NodeKind::Router { fields });
                // Set current_value to ObjectHandle pointing to self
                node.extension_mut().current_value = Some(Payload::ObjectHandle(router_slot));
            }

            zoon::println!("    Restored Object at slot {:?} with {} fields",
                router_slot, fields_json.len());

            Some(router_slot)
        }
        // For scalar types, create a Producer
        "Number" | "Text" | "Bool" | "Unit" | "Tag" => {
            let payload = json_to_payload(json)?;
            let slot = event_loop.arena.alloc();
            if let Some(node) = event_loop.arena.get_mut(slot) {
                node.set_kind(NodeKind::Producer {
                    value: Some(payload.clone()),
                });
                node.extension_mut().current_value = Some(payload);
            }
            Some(slot)
        }
        _ => None,
    }
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

/// Default localStorage key for engine-v2 state persistence.
pub const STATES_STORAGE_KEY: &str = "boon-playground-v2-states";

/// Run Boon code and return a Zoon element for the playground.
///
/// This is the entry point for the playground UI with engine-v2.
/// Takes ownership of the source code to avoid lifetime issues.
pub fn run_and_render(source_code: String) -> impl Element + use<> {
    run_and_render_with_storage(source_code, Some(STATES_STORAGE_KEY.to_string()))
}

/// Run Boon code with optional state persistence.
pub fn run_and_render_with_storage(source_code: String, storage_key: Option<String>) -> impl Element + use<> {
    // Invalidate all running timers from previous code BEFORE running new code.
    // This is critical for scalar results that don't create a ReactiveEventLoop.
    invalidate_all_timers();

    let result = run_v2_with_storage(&source_code, storage_key.as_deref());

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
            let reactive_el = ReactiveEventLoop::new_with_storage(r.event_loop, r.root_slot, storage_key);

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
