//! DD Bridge - Converts DD values to Zoon elements.
//!
//! This module provides functions to render `Value` as Zoon elements.
//! Currently implements static rendering; reactive rendering will use
//! Output streams in a future phase.

use std::sync::Arc;

use super::super::eval::interpreter::{DdContext, DdResult};
use super::super::core::types::{BoolTag, ElementTag};
use super::super::core::value::Value;
use zoon::*;

use super::super::io::{fire_global_link, fire_global_link_with_bool, fire_global_blur, fire_global_key_down, cell_states_signal, get_cell_value, add_dynamic_link_action, DynamicLinkAction, sync_cell_from_dd};
// Phase 7: toggle_cell_bool, update_cell_no_persist removed - runtime updates flow through DD
// Phase 7: find_template_hover_link, find_template_hover_cell removed - symbolic refs eliminated
use std::sync::atomic::{AtomicU32, Ordering};

/// Helper function to get the variant name of a Value for debug logging.
fn dd_value_variant_name(value: &Value) -> &'static str {
    match value {
        Value::Unit => "Unit",
        Value::Bool(_) => "Bool",
        Value::Number(_) => "Number",
        Value::Text(_) => "Text",
        Value::List(_) => "List",
        Value::Collection(_) => "Collection",
        Value::Object(_) => "Object",
        Value::Tagged { tag, .. } => {
            // For Tagged, we want to show the tag name, but we can't return a dynamic string
            // So we'll just return "Tagged" and log the tag separately
            "Tagged"
        }
        Value::CellRef(_) => "CellRef",
        Value::LinkRef(_) => "LinkRef",
        Value::TimerRef { .. } => "TimerRef",
        Value::WhileRef { .. } => "WhileRef",
        Value::ComputedRef { .. } => "ComputedRef",
        Value::FilteredListRef { .. } => "FilteredListRef",
        Value::ReactiveFilteredList { .. } => "ReactiveFilteredList",
        Value::ReactiveText { .. } => "ReactiveText",
        Value::Placeholder => "Placeholder",
        Value::PlaceholderField { .. } => "PlaceholderField",
        Value::PlaceholderWhileRef { .. } => "PlaceholderWhileRef",
        Value::NegatedPlaceholderField { .. } => "NegatedPlaceholderField",
        Value::MappedListRef { .. } => "MappedListRef",
        Value::FilteredMappedListRef { .. } => "FilteredMappedListRef",
        Value::FilteredListRefWithPredicate { .. } => "FilteredListRefWithPredicate",
        Value::FilteredMappedListWithPredicate { .. } => "FilteredMappedListWithPredicate",
        Value::LatestRef { .. } => "LatestRef",
        Value::Flushed(_) => "Flushed",
    }
}

/// Get the current value of the focused text input via DOM access.
/// This is used when Enter is pressed to capture the input text.
/// We try multiple methods to ensure we get the value (CDP typing can be tricky).
#[cfg(target_arch = "wasm32")]
fn get_dd_text_input_value() -> String {
    use zoon::*;

    // Method 1: Try activeElement first (preferred for editing inputs where multiple inputs exist)
    let active = document().active_element();
    let active_tag = active.as_ref().map(|el| el.tag_name()).unwrap_or_default();
    let active_value = active
        .and_then(|el| el.dyn_into::<web_sys::HtmlInputElement>().ok())
        .map(|input| input.value())
        .unwrap_or_default();

    if !active_value.is_empty() {
        zoon::println!("[DD TextInput] get_dd_text_input_value: active_tag={}, value='{}'", active_tag, active_value);
        return active_value;
    }

    // Method 2: Try reading by ID
    let id_value = document()
        .get_element_by_id("dd_text_input")
        .and_then(|el| el.dyn_into::<web_sys::HtmlInputElement>().ok())
        .map(|input| input.value())
        .unwrap_or_default();

    if !id_value.is_empty() {
        zoon::println!("[DD TextInput] get_dd_text_input_value: fallback_id='{}'", id_value);
        return id_value;
    }

    // Method 3: Use JavaScript evaluation as final fallback (works around CDP quirks)
    // CDP's Input.insertText may not update the DOM value property synchronously
    let js_result = js_sys::eval(r#"
        (function() {
            var el = document.activeElement;
            if (el && el.tagName === 'INPUT') return el.value || '';
            el = document.getElementById('dd_text_input');
            if (el) return el.value || '';
            return '';
        })()
    "#);

    let js_value = js_result
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();

    zoon::println!("[DD TextInput] get_dd_text_input_value: active_tag={}, js_eval='{}'", active_tag, js_value);
    js_value
}

/// Clear the DD text input value via DOM access.
/// This implements the Boon pattern: text_to_add |> THEN { Text/empty() }
/// Called when the text_input_text HOLD is updated after a successful List/append.
#[cfg(target_arch = "wasm32")]
pub fn clear_dd_text_input_value() {
    use zoon::*;
    if let Some(el) = document().get_element_by_id("dd_text_input") {
        if let Ok(input) = el.dyn_into::<web_sys::HtmlInputElement>() {
            input.set_value("");
        }
    }
}

// REMOVED: get_dynamic_item_edit_value - dead code (render_dynamic_item was removed)

/// Convert a Value Oklch color to CSS color string.
/// Returns None if the color should be invisible (alpha=0 or broken WhileRef).
/// Evaluate a WhileRef with computation to get the current value.
/// Returns the matching arm's body value or None if no match.
fn evaluate_while_ref_now(cell_id: &super::super::core::types::CellId, computation: &Option<super::super::core::value::ComputedType>, arms: &[(Value, Value)]) -> Option<Value> {
    use super::super::core::value::evaluate_computed;

    // Get the source value from hold states
    let source_value = get_cell_value(&cell_id.name()).unwrap_or(Value::Unit);

    // If there's a computation, evaluate it to get the actual value to match
    let value_to_match = if let Some(comp) = computation {
        evaluate_computed(comp, &source_value)
    } else {
        source_value
    };

    // Match against arms
    for (pattern, body) in arms.iter() {
        let matches = match (&value_to_match, pattern) {
            (&Value::Bool(b), Value::Tagged { tag, .. }) => {
                BoolTag::matches_bool(tag.as_ref(), b)
            }
            (Value::Text(curr), Value::Text(pat)) => curr == pat,
            (Value::Tagged { tag: curr_tag, .. }, Value::Tagged { tag: pat_tag, .. }) => curr_tag == pat_tag,
            _ => &value_to_match == pattern,
        };
        if matches {
            return Some(body.clone());
        }
    }
    None
}

/// Evaluate a Value for filter comparison, resolving CellRefs and WhileRefs.
/// Used by FilteredMappedListRef to compare field values with filter values.
/// Recursively evaluates until we get a concrete value (Bool, Tagged, etc.).
fn evaluate_dd_value_for_filter(value: &Value, states: &std::collections::HashMap<String, Value>) -> Value {
    match value {
        Value::CellRef(cell_id) => {
            let resolved = states.get(&cell_id.name()).cloned().unwrap_or(Value::Unit);
            // Recursively evaluate in case the hold value is itself a CellRef or WhileRef
            evaluate_dd_value_for_filter(&resolved, states)
        }
        Value::WhileRef { cell_id, computation, arms, default } => {
            let result = evaluate_while_ref_now(cell_id, computation, arms)
                .or_else(|| default.as_ref().map(|d| (**d).clone()))
                .unwrap_or(Value::Unit);
            // Recursively evaluate in case the WhileRef body is a CellRef
            evaluate_dd_value_for_filter(&result, states)
        }
        other => other.clone(),
    }
}

fn dd_oklch_to_css(value: &Value) -> Option<String> {
    match value {
        Value::Tagged { tag, fields } if tag.as_ref() == "Oklch" => {
            // Handle lightness - can be Number or WhileRef (reactive)
            let lightness = match fields.get("lightness") {
                Some(Value::Number(n)) => n.0,
                Some(Value::WhileRef { cell_id, computation, arms, .. }) => {
                    // Evaluate WhileRef to get current lightness value
                    let result = evaluate_while_ref_now(cell_id, computation, arms);
                    zoon::println!("[DD dd_oklch_to_css] WhileRef lightness: cell_id={}, computation={:?}, result={:?}", cell_id, computation.is_some(), result);
                    match result {
                        Some(Value::Number(n)) => n.0,
                        _ => 0.5, // default
                    }
                }
                _ => 0.5, // default
            };
            let chroma = fields.get("chroma")
                .and_then(|v| if let Value::Number(n) = v { Some(n.0) } else { None })
                .unwrap_or(0.0);
            let hue = fields.get("hue")
                .and_then(|v| if let Value::Number(n) = v { Some(n.0) } else { None })
                .unwrap_or(0.0);

            // Handle alpha - can be Number or WhileRef
            let alpha_value = fields.get("alpha");
            let alpha = match alpha_value {
                Some(Value::Number(n)) => Some(n.0),
                Some(Value::WhileRef { cell_id, arms, .. }) => {
                    // Try to evaluate WhileRef based on cell state
                    // If arms is empty, it's a broken WhileRef - use default alpha (0.4 for selected state)
                    if arms.is_empty() {
                        zoon::println!("[DD Bridge] dd_oklch_to_css: WhileRef alpha has empty arms, using default alpha 0.4");
                        return Some(format!("oklch({}% {} {} / 0.4)", lightness * 100.0, chroma, hue));
                    }
                    // Try to get current value from cell_states and match against arms
                    let current = get_cell_value(&cell_id.name());
                    if let Some(current_val) = current {
                        for (pattern, body) in arms.iter() {
                            let matches = match (&current_val, pattern) {
                                (Value::Text(curr), Value::Text(pat)) => curr == pat,
                                (Value::Bool(b), Value::Tagged { tag, .. }) =>
                                    BoolTag::matches_bool(tag.as_ref(), *b),
                                _ => &current_val == pattern,
                            };
                            if matches {
                                if let Value::Number(n) = body {
                                    return if n.0 == 0.0 {
                                        None  // alpha=0 means invisible
                                    } else {
                                        Some(format!("oklch({}% {} {} / {})", lightness * 100.0, chroma, hue, n.0))
                                    };
                                }
                            }
                        }
                    }
                    None  // No match, default to invisible
                }
                _ => None,
            };

            // oklch(lightness% chroma hue / alpha)
            if let Some(a) = alpha {
                Some(format!("oklch({}% {} {} / {})", lightness * 100.0, chroma, hue, a))
            } else {
                Some(format!("oklch({}% {} {})", lightness * 100.0, chroma, hue))
            }
        }
        _ => None,
    }
}

/// Render a DD document as a Zoon element.
///
/// # Arguments
///
/// * `document` - The document value from DD evaluation
/// * `_context` - The evaluation context (unused in static rendering)
///
/// # Returns
///
/// A Zoon element representing the document.
pub fn render_dd_document_reactive_signal(
    document: Value,
    _context: DdContext,
) -> impl Element {
    render_dd_value(&document)
}

/// Render a DD result as a Zoon element.
///
/// # Arguments
///
/// * `result` - The full DD result including document and context
///
/// # Returns
///
/// A Zoon element representing the result.
pub fn render_dd_result_reactive_signal(
    result: DdResult,
) -> impl Element {
    match result.document {
        Some(doc) => render_dd_value(&doc).unify(),
        None => El::new()
            .s(Font::new().color(color!("LightCoral")))
            .child("DD Engine: No document produced")
            .unify(),
    }
}

/// Render a Value as a Zoon element.
fn render_dd_value(value: &Value) -> RawElOrText {
    match value {
        Value::Unit => El::new().unify(),

        Value::Bool(b) => Text::new(if *b { "true" } else { "false" }).unify(),

        Value::Number(n) => {
            // Format number nicely (no trailing .0 for integers)
            let num = n.0;
            let text = if num.fract() == 0.0 {
                format!("{}", num as i64)
            } else {
                format!("{}", num)
            };
            Text::new(text).unify()
        }

        Value::Text(s) => {
            Text::new(s.to_string()).unify()
        }

        Value::List(items) => {
            // Render list as column of items
            let children: Vec<RawElOrText> = items.iter().map(|item| render_dd_value(item)).collect();
            Column::new()
                .items(children)
                .unify()
        }

        Value::Collection(handle) => {
            // Render DD collection the same as List (from snapshot)
            let children: Vec<RawElOrText> = handle.iter().map(|item| render_dd_value(item)).collect();
            Column::new()
                .items(children)
                .unify()
        }

        Value::Object(fields) => {
            // Render object as debug representation
            let debug = fields
                .iter()
                .map(|(k, v)| format!("{}: {:?}", k, v))
                .collect::<Vec<_>>()
                .join(", ");
            Text::new(format!("[{}]", debug)).unify()
        }

        Value::ReactiveText { parts } => {
            // Reactive TEXT with interpolated values - evaluated at render time
            let parts = parts.clone();
            El::new()
                .child_signal(
                    cell_states_signal()
                        .map(move |_states| {
                            // Evaluate all parts with current HOLD state
                            let result: String = parts.iter()
                                .map(|part| part.to_display_string())
                                .collect();
                            Text::new(result)
                        })
                )
                .unify()
        }

        Value::Tagged { tag, fields } => {
            zoon::println!("[DD render_dd_value] Tagged(tag='{}', fields={:?})", tag, fields.keys().collect::<Vec<_>>());
            render_tagged_element(tag.as_ref(), fields)
        }

        Value::CellRef(name) => {
            // CellRef is a reactive reference to a HOLD value
            // Observe cell_states_signal and render current value reactively
            let cell_id = name.to_string();

            // Create reactive element that updates when CELL_STATES change
            El::new()
                .child_signal(
                    cell_states_signal()
                        .map(move |states| {
                            let text = states
                                .get(&cell_id)
                                .map(|v| v.to_display_string())
                                .unwrap_or_else(|| "?".to_string());
                            Text::new(text)
                        })
                )
                .unify()
        }

        Value::LinkRef(link_id) => {
            // LinkRef is a placeholder for an event source
            // In static rendering, show as unit (events are wired at button level)
            El::new().unify()
        }

        Value::TimerRef { id, interval_ms: _ } => {
            // TimerRef represents a timer-driven HOLD accumulator
            // The `id` is the HOLD id - render its reactive value
            let cell_id = id.to_string();

            // Create reactive element that updates when CELL_STATES change
            // NOTE: Returns empty string if HOLD hasn't been set yet (timer not fired)
            El::new()
                .child_signal(
                    cell_states_signal()
                        .map(move |states| {
                            let text = states
                                .get(&cell_id)
                                .map(|v| v.to_display_string())
                                .unwrap_or_default(); // Empty until first timer tick
                            Text::new(text)
                        })
                )
                .unify()
        }

        Value::WhileRef { cell_id, computation, arms, default } => {
            // WhileRef is a reactive WHILE/WHEN expression
            // Observe the cell value and render the matching arm
            let cell_id = cell_id.to_string();
            let computation = computation.clone();
            let arms = arms.clone();
            let default = default.clone();

            // Debug: log the arms Arc address and first arm body to detect sharing
            let arms_ptr = Arc::as_ptr(&arms) as usize;
            let first_arm_summary = arms.first().map(|(pattern, body)| {
                // Check if body is a button with a press LinkRef
                fn find_press_link(v: &Value) -> Option<String> {
                    match v {
                        Value::Tagged { tag, fields } if ElementTag::is_element(tag.as_ref()) => {
                            if let Some(element) = fields.get("element") {
                                if let Some(event) = element.get("event") {
                                    if let Some(Value::LinkRef(id)) = event.get("press") {
                                        return Some(id.to_string());
                                    }
                                }
                            }
                            None
                        }
                        _ => None,
                    }
                }
                format!("pattern={:?}, body_press={:?}", pattern, find_press_link(body))
            });

            // Create reactive element that updates when relevant hold values change
            // Use map + dedupe to avoid re-rendering on unrelated hold changes (like hover)
            //
            // For ListCountHold/ListCountWhereHold: we need to watch ALL holds, not just the source_hold.
            // This is because the count depends on the list items in the HOLD (which can be dynamically added).
            // For ListCountWhereHold specifically, the count also depends on CellRef values inside items (e.g., checkbox states).
            fn contains_list_count_hold(comp: &super::super::core::value::ComputedType) -> bool {
                use super::super::core::value::ComputedType;
                match comp {
                    // List computations that need to watch all holds
                    ComputedType::ListCountWhereHold { .. } => true,
                    ComputedType::ListCountHold { .. } => true,
                    ComputedType::ListIsEmptyHold { .. } => true,
                    ComputedType::GreaterThanZero { operand } => {
                        if let Value::ComputedRef { computation: inner_comp, .. } = operand.as_ref() {
                            contains_list_count_hold(inner_comp)
                        } else {
                            false
                        }
                    }
                    ComputedType::Equal { left, right } => {
                        // Check if either operand contains ListCountHold/ListCountWhereHold
                        let left_has = if let Value::ComputedRef { computation: inner_comp, .. } = left.as_ref() {
                            contains_list_count_hold(inner_comp)
                        } else {
                            false
                        };
                        let right_has = if let Value::ComputedRef { computation: inner_comp, .. } = right.as_ref() {
                            contains_list_count_hold(inner_comp)
                        } else {
                            false
                        };
                        left_has || right_has
                    }
                    _ => false,
                }
            }
            let needs_watch_all_cells = computation.as_ref().map_or(false, contains_list_count_hold);
            let cell_id_for_extract = cell_id.clone();
            let cell_id_for_log = cell_id.clone();
            El::new()
                .child_signal(
                    cell_states_signal()
                        .map(move |states| {
                            let main_value = states.get(&cell_id_for_extract).cloned();
                            if needs_watch_all_cells {
                                // For ListCountHold/ListCountWhereHold: include all states so any cell change triggers re-evaluation
                                // This is necessary because the count depends on cell contents (dynamic list additions/removals)
                                (main_value, Some(states))
                            } else {
                                // For other computations: only watch the main cell
                                (main_value, None)
                            }
                        })
                        .dedupe_cloned()  // Emit when watched cell values change
                        .map(move |(source_value, _watched_source)| {
                            zoon::println!("[DD WhileRef RENDER] cell_id={}, value={:?}", cell_id_for_log, source_value);
                            let source_value = source_value.as_ref();

                            // Determine the value to match against patterns
                            // If there's a computation, evaluate it first
                            let current_value: Option<Value> = if let Some(ref comp) = computation {
                                // Evaluate the computation to get a boolean
                                if let Some(source) = source_value {
                                    use super::super::core::value::evaluate_computed;
                                    let result = evaluate_computed(comp, source);
                                    Some(result)
                                } else {
                                    None
                                }
                            } else {
                                source_value.cloned()
                            };


                            // Find matching arm based on current value
                            if let Some(ref current) = current_value {
                                // For text values (like route paths), match against text patterns
                                for (pattern, body) in arms.iter() {
                                    let matches = match (current, pattern) {
                                        // Bool to True/False tag comparison
                                        (Value::Bool(b), Value::Tagged { tag, .. }) => {
                                            BoolTag::matches_bool(tag.as_ref(), *b)
                                        }
                                        // Text to text comparison
                                        (Value::Text(curr), Value::Text(pat)) => curr == pat,
                                        // Tag comparison (e.g., Home, About)
                                        (Value::Tagged { tag: curr_tag, .. }, Value::Tagged { tag: pat_tag, .. }) => curr_tag == pat_tag,
                                        // Text to tag comparison - compare the text with tag name (case-insensitive)
                                        // For route matching, "/" maps to root tag, "/foo" maps to "Foo" tag
                                        (Value::Text(text), Value::Tagged { tag, .. }) => {
                                            let text_ref = text.as_ref();
                                            let tag_ref = tag.as_ref();
                                            // Root path "/" or "" matches tag if tag is the root tag name
                                            if text_ref == "/" || text_ref.is_empty() {
                                                // Check if tag matches common root names
                                                tag_ref.eq_ignore_ascii_case("home") ||
                                                tag_ref.eq_ignore_ascii_case("root") ||
                                                tag_ref.eq_ignore_ascii_case("all")
                                            } else {
                                                // "/foo" should match "Foo" tag (strip leading /, compare case-insensitive)
                                                let route_name = text_ref.trim_start_matches('/');
                                                route_name.eq_ignore_ascii_case(tag_ref)
                                            }
                                        }
                                        _ => current == pattern,
                                    };

                                    if matches {
                                        return render_dd_value(body);
                                    }
                                }
                            }

                            // No match - use default if available
                            if let Some(ref def) = default {
                                // If we have a computed numeric value and the default is text,
                                // render the number followed by the default text (generic approach)
                                if let (Some(Value::Number(n)), Value::Text(text)) = (&current_value, def.as_ref()) {
                                    // Render: "N" + default_text (e.g., "2" + " items left" = "2 items left")
                                    let text_str = text.to_string();
                                    // Clean up any placeholder markers in the text
                                    let clean_text = text_str
                                        .replace("[while:", "")
                                        .replace("]", "");
                                    return Text::new(format!("{}{}", n.0 as i64, clean_text)).unify();
                                }
                                return render_dd_value(def.as_ref());
                            }

                            // No match and no default - render empty
                            El::new().unify()
                        })
                )
                .unify()
        }

        Value::ComputedRef { computation, source_hold } => {
            // ComputedRef is a reactive computed value that depends on a HOLD
            // Observe cell_states_signal and re-evaluate computation when source changes
            use super::super::core::value::evaluate_computed;

            let source_hold = source_hold.to_string();
            let computation = computation.clone();

            El::new()
                .child_signal(
                    cell_states_signal()
                        .map(move |states| {
                            // Get source HOLD value
                            let source_value = states.get(&source_hold)
                                .cloned()
                                .unwrap_or(Value::Unit);

                            // Evaluate the computation
                            let result = evaluate_computed(&computation, &source_value);

                            // Render the result as text
                            Text::new(result.to_display_string())
                        })
                )
                .unify()
        }

        Value::FilteredListRef { source_hold, filter_field, filter_value: _ } => {
            // FilteredListRef is an intermediate value - shouldn't normally be rendered directly
            // If it is rendered, show debug info
            Text::new(format!("[filtered:{}@{}]", filter_field, source_hold)).unify()
        }

        Value::ReactiveFilteredList { items, filter_field, filter_value: _, cell_ids: _, source_hold: _ } => {
            // ReactiveFilteredList is an intermediate value - shouldn't normally be rendered directly
            // If it is rendered, show debug info
            Text::new(format!("[reactive-filtered:{}#{}]", filter_field, items.len())).unify()
        }

        Value::FilteredListRefWithPredicate { source_hold, .. } => {
            // FilteredListRefWithPredicate is an intermediate value - shouldn't normally be rendered directly
            // If it is rendered, show debug info
            Text::new(format!("[filtered-list-predicate:{}]", source_hold)).unify()
        }

        Value::Placeholder => {
            // Placeholder should never be rendered directly - it's a template marker
            Text::new("[placeholder]").unify()
        }

        Value::PlaceholderField { path } => {
            // PlaceholderField should never be rendered directly - it's a deferred field access marker
            Text::new(format!("[placeholder.{}]", path.join("."))).unify()
        }

        Value::PlaceholderWhileRef { field_path, .. } => {
            // PlaceholderWhileRef should never be rendered directly - it's a deferred WHILE marker
            Text::new(format!("[placeholder-while.{}]", field_path.join("."))).unify()
        }

        Value::NegatedPlaceholderField { path } => {
            // NegatedPlaceholderField should never be rendered directly - it's a deferred negation marker
            Text::new(format!("[not-placeholder.{}]", path.join("."))).unify()
        }

        Value::MappedListRef { source_hold, element_template } => {
            // MappedListRef should be handled in render_stripe; if rendered directly, show reactive column
            zoon::println!("[DD render_dd_value] MappedListRef: source_hold={}", source_hold);
            let source_hold = source_hold.clone();
            let element_template = element_template.clone();
            El::new()
                .child_signal(
                    cell_states_signal()
                        .map(move |states| {
                            let items = states.get(&source_hold.name());
                            if let Some(list_items) = items.and_then(|v| v.as_list_items()) {
                                zoon::println!("[DD MappedListRef direct] rendering {} items", list_items.len());
                                let children: Vec<RawElOrText> = list_items.iter().map(|item| {
                                    let concrete_element = element_template.substitute_placeholder(item);
                                    render_dd_value(&concrete_element)
                                }).collect();
                                Column::new().items(children).unify_option()
                            } else {
                                None
                            }
                        })
                )
                .unify()
        }

        Value::FilteredMappedListRef { source_hold, filter_field, filter_value, element_template } => {
            // FilteredMappedListRef: render filtered then mapped list from HOLD
            zoon::println!("[DD render_dd_value] FilteredMappedListRef: source_hold={}, filter={}", source_hold, filter_field);
            let source_hold = source_hold.clone();
            let filter_field = filter_field.clone();
            let filter_value = filter_value.clone();
            let element_template = element_template.clone();
            El::new()
                .child_signal(
                    cell_states_signal()
                        .map(move |states| {
                            let items = states.get(&source_hold.name());
                            if let Some(list_items) = items.and_then(|v| v.as_list_items()) {
                                // Filter items by field value
                                let filtered: Vec<&Value> = list_items.iter().filter(|item| {
                                    if let Value::Object(obj) = item {
                                        if let Some(field_val) = obj.get(filter_field.as_ref()) {
                                            // Evaluate the field value (might be a CellRef or WhileRef)
                                            let evaluated = evaluate_dd_value_for_filter(field_val, &states);
                                            let filter_eval = evaluate_dd_value_for_filter(&filter_value, &states);
                                            return evaluated == filter_eval;
                                        }
                                    }
                                    false
                                }).collect();
                                zoon::println!("[DD FilteredMappedListRef direct] filtered {} -> {} items",
                                    list_items.len(), filtered.len());
                                let children: Vec<RawElOrText> = filtered.iter().map(|item| {
                                    let concrete_element = element_template.substitute_placeholder(item);
                                    render_dd_value(&concrete_element)
                                }).collect();
                                Column::new().items(children).unify_option()
                            } else {
                                None
                            }
                        })
                )
                .unify()
        }

        Value::FilteredMappedListWithPredicate { source_hold, predicate_template, element_template } => {
            // FilteredMappedListWithPredicate: render list with generic predicate filtering
            // The predicate_template contains Placeholder markers that get substituted with each item
            let source_hold = source_hold.clone();
            let predicate_template = predicate_template.clone();
            let element_template = element_template.clone();
            El::new()
                .child_signal(
                    cell_states_signal()
                        .map(move |states| {
                            let items = states.get(&source_hold.name());
                            if let Some(list_items) = items.and_then(|v| v.as_list_items()) {
                                // Filter items by evaluating predicate for each
                                let filtered: Vec<&Value> = list_items.iter().filter(|item| {
                                    let resolved_predicate = predicate_template.substitute_placeholder(item);
                                    let evaluated = evaluate_dd_value_for_filter(&resolved_predicate, &states);
                                    evaluated.is_truthy()
                                }).collect();
                                let children: Vec<RawElOrText> = filtered.iter().map(|item| {
                                    let concrete_element = element_template.substitute_placeholder(item);
                                    render_dd_value(&concrete_element)
                                }).collect();
                                Column::new().items(children).unify_option()
                            } else {
                                None
                            }
                        })
                )
                .unify()
        }

        Value::LatestRef { initial, .. } => {
            // LatestRef should have been processed by Math/sum() or Router/go_to()
            // If we reach here, just render the initial value
            zoon::println!("[DD render_dd_value] LatestRef reached render - using initial value");
            render_dd_value(initial)
        }
        Value::Flushed(inner) => {
            // Flushed values propagate through rendering - render the inner value
            // In actual FLUSH blocks, this would be caught and handled differently
            zoon::println!("[DD render_dd_value] Flushed value reached render - propagating inner");
            render_dd_value(inner)
        }
    }
}

/// Render a tagged object as a Zoon element.
fn render_tagged_element(tag: &str, fields: &Arc<std::collections::BTreeMap<Arc<str>, Value>>) -> RawElOrText {
    zoon::println!("[DD render_tagged] tag='{}', fields={:?}", tag, fields.keys().collect::<Vec<_>>());
    match tag {
        "Element" => render_element(fields),
        "NoElement" => El::new().unify(),
        _ => {
            // Unknown tag - render as text
            zoon::println!("[DD render_tagged] UNKNOWN tag '{}' - rendering as text", tag);
            Text::new(format!("{}[...]", tag)).unify()
        }
    }
}

/// Render an Element tagged object.
fn render_element(fields: &Arc<std::collections::BTreeMap<Arc<str>, Value>>) -> RawElOrText {
    let element_type = fields
        .get("_element_type")
        .and_then(|v| match v {
            Value::Text(s) => Some(s.as_ref()),
            _ => None,
        })
        .unwrap_or("container");

    zoon::println!("[DD render_element] type='{}', all_fields={:?}", element_type, fields.keys().collect::<Vec<_>>());

    match element_type {
        "button" => render_button(fields),
        "stripe" => {
            zoon::println!("[DD render_element] -> render_stripe()");
            render_stripe(fields)
        }
        "stack" => render_stack(fields),
        "container" => render_container(fields),
        "text_input" => render_text_input(fields),
        "checkbox" => {
            zoon::println!("[DD render_element] -> render_checkbox()");
            render_checkbox(fields)
        }
        "label" => {
            zoon::println!("[DD render_element] -> render_label()");
            render_label(fields)
        }
        "paragraph" => render_paragraph(fields),
        "link" => render_link(fields),
        _ => {
            zoon::println!("[DD render_element] UNKNOWN type '{}' - rendering as container", element_type);
            // Unknown element type - render as container
            render_container(fields)
        }
    }
}

/// Render a button element.
fn render_button(fields: &Arc<std::collections::BTreeMap<Arc<str>, Value>>) -> RawElOrText {
    let label = fields
        .get("label")
        .map(|v| v.to_display_string())
        .unwrap_or_default();

    // Extract LinkRef from element.event.press if present
    let element_value = fields.get("element");
    let event_value = element_value.and_then(|e| e.get("event"));
    let press_value = event_value.and_then(|e| e.get("press"));
    zoon::println!("[DD render_button] label='{}' element={:?} event={:?} press={:?}",
        label,
        element_value.map(|v| format!("{:?}", v)).unwrap_or_else(|| "None".to_string()),
        event_value.map(|v| format!("{:?}", v)).unwrap_or_else(|| "None".to_string()),
        press_value.map(|v| format!("{:?}", v)).unwrap_or_else(|| "None".to_string()));
    let link_id = press_value
        .and_then(|v| match v {
            Value::LinkRef(id) => Some(id.to_string()),
            _ => None,
        });
    zoon::println!("[DD render_button] Extracted link_id={:?}", link_id);

    // Extract outline from style.outline
    // Note: outline may be a WhileRef for reactive styling based on selection state
    let style_value = fields.get("style");
    let outline_value = style_value.and_then(|s| s.get("outline"));

    // Check if outline is a WhileRef (reactive) - need to render reactively
    let is_reactive_outline = matches!(outline_value, Some(Value::WhileRef { .. }));

    let outline_opt: Option<Outline> = outline_value
        .and_then(|outline| {
            match outline {
                Value::Tagged { tag, .. } if tag.as_ref() == "NoOutline" => None,
                Value::Object(obj) => {
                    // Get color from outline object
                    let css_color = obj.get("color").and_then(|c| dd_oklch_to_css(c));
                    if let Some(color) = css_color {
                        // Check for side: Inner vs outer (default outer)
                        let is_inner = obj.get("side")
                            .map(|s| matches!(s, Value::Tagged { tag, .. } if tag.as_ref() == "Inner"))
                            .unwrap_or(false);
                        // Get width (default 1)
                        let width = obj.get("width")
                            .and_then(|w| if let Value::Number(n) = w { Some(n.0 as u32) } else { None })
                            .unwrap_or(1);
                        // Build outline
                        let outline = if is_inner {
                            Outline::inner().width(width).solid().color(color)
                        } else {
                            Outline::outer().width(width).solid().color(color)
                        };
                        Some(outline)
                    } else {
                        None
                    }
                }
                _ => None,
            }
        });

    // Extract font styling from style.font
    let font_color_css = style_value
        .and_then(|s| s.get("font"))
        .and_then(|f| f.get("color"))
        .and_then(|c| dd_oklch_to_css(c));
    let font_size = style_value
        .and_then(|s| s.get("font"))
        .and_then(|f| f.get("size"))
        .and_then(|v| if let Value::Number(n) = v { Some(n.0 as u32) } else { None });

    // Build button with optional outline and font styling
    let mut button = Button::new().label(label.clone());

    // Apply font styling
    let mut font = Font::new();
    if let Some(color) = font_color_css {
        font = font.color(color);
    }
    if let Some(size) = font_size {
        font = font.size(size);
    }
    button = button.s(font);

    if let Some(outline) = outline_opt {
        button = button.s(outline);
    } else if is_reactive_outline {
        // For reactive outline (WhileRef), wrap button in reactive container
        // For now, apply transparent outline as default to override Zoon's default button styling
        button = button.s(Outline::outer().width(0).color("transparent"));
    } else if outline_value.is_some() {
        // Outline was specified but didn't match any pattern - apply no-outline
        button = button.s(Outline::outer().width(0).color("transparent"));
    }

    if let Some(link_id) = link_id {
        // Wire button to fire the link event via global dispatcher
        button
            .on_press(move || {
                fire_global_link(&link_id);
            })
            .unify()
    } else {
        button.unify()
    }
}

/// Render a stripe (vertical/horizontal layout).
fn render_stripe(fields: &Arc<std::collections::BTreeMap<Arc<str>, Value>>) -> RawElOrText {
    let direction = fields
        .get("direction")
        .and_then(|v| match v {
            Value::Tagged { tag, .. } => Some(tag.as_ref().to_string()),
            Value::Text(s) => Some(s.to_string()),
            _ => None,
        })
        .unwrap_or_else(|| "Column".to_string());

    let gap = fields
        .get("gap")
        .and_then(|v| match v {
            Value::Number(n) => Some(n.0 as u32),
            _ => None,
        })
        .unwrap_or(0);

    // Extract hovered LinkRef from element.hovered if present
    let hovered_link_id = fields
        .get("element")
        .and_then(|e| e.get("hovered"))
        .and_then(|v| match v {
            Value::LinkRef(id) => Some(id.to_string()),
            _ => None,
        });

    // Check if this is a list (Ul tag) - needs reactive filtering
    let element_tag = fields
        .get("element")
        .and_then(|e| e.get("tag"));
    let items_value = fields.get("items");

    // Debug: what type is items_value?
    if let Some(iv) = items_value {
        zoon::println!("[DD render_stripe DEBUG] items_value variant={}", dd_value_variant_name(iv));
    }

    // MappedListRef: Reactive list rendering from HOLD with template substitution
    // Detects hover patterns in the template and remaps to unique IDs per item
    if let Some(Value::MappedListRef { source_hold, element_template }) = items_value {
        zoon::println!("[DD render_stripe] MappedListRef: source_hold={}", source_hold);
        let source_hold = source_hold.clone();
        let element_template = element_template.clone();

        // Find original hover link/cell from template for remapping
        let original_hover_link = find_template_hover_link(&element_template);
        let original_hover_cell = find_template_hover_cell(&element_template);
        zoon::println!("[DD MappedListRef] Template hover detection: link={:?}, cell={:?}",
            original_hover_link, original_hover_cell);

        return El::new()
            .child_signal(
                cell_states_signal()
                    .map(move |states| {
                        let items = states.get(&source_hold.name());
                        if let Some(list_items) = items.and_then(|v| v.as_list_items()) {
                            zoon::println!("[DD MappedListRef] rendering {} items from HOLD", list_items.len());
                            let children: Vec<RawElOrText> = list_items.iter().enumerate().map(|(idx, item)| {
                                // Generate unique hover IDs for this item if template has hover pattern
                                let concrete_element = if original_hover_link.is_some() && original_hover_cell.is_some() {
                                    // Use STABLE hover IDs based on source_hold name + item index
                                    // This ensures the same item gets the same hover ID across re-renders
                                    let new_hover_link = format!("mapped_hover_{}_{}", source_hold.name(), idx);
                                    let new_hover_cell = format!("hover_{}", new_hover_link);

                                    // Register HoverState action for this item
                                    // NOTE: Do NOT call update_cell_no_persist here - we're inside a signal callback!
                                    // Updating CELL_STATES inside cell_states_signal().map() causes infinite recursion.
                                    // The hover cell will be lazily initialized when the first hover event fires.
                                    add_dynamic_link_action(
                                        new_hover_link.clone(),
                                        DynamicLinkAction::HoverState(new_hover_cell.clone())
                                    );

                                    zoon::println!("[DD MappedListRef] Item {}: remapped hover {} -> {}, cell {} -> {}",
                                        idx, original_hover_link.as_ref().unwrap(), new_hover_link,
                                        original_hover_cell.as_ref().unwrap(), new_hover_cell);

                                    // Substitute with hover remapping
                                    element_template.substitute_placeholder_with_hover_remap(
                                        item,
                                        &new_hover_link,
                                        &new_hover_cell,
                                        original_hover_link.as_deref(),
                                        original_hover_cell.as_deref(),
                                    )
                                } else {
                                    // No hover pattern, use simple substitution
                                    element_template.substitute_placeholder(item)
                                };
                                render_dd_value(&concrete_element)
                            }).collect();
                            Some(Column::new().s(Gap::new().y(gap)).items(children).unify())
                        } else {
                            zoon::println!("[DD MappedListRef] no items or wrong type in HOLD");
                            None
                        }
                    })
            )
            .unify();
    }

    // FilteredMappedListRef: Reactive filtered + mapped list rendering from HOLD
    if let Some(Value::FilteredMappedListRef { source_hold, filter_field, filter_value, element_template }) = items_value {
        zoon::println!("[DD render_stripe] FilteredMappedListRef: source_hold={}, filter={}", source_hold, filter_field);
        let source_hold = source_hold.clone();
        let filter_field = filter_field.clone();
        let filter_value = filter_value.clone();
        let element_template = element_template.clone();
        return El::new()
            .child_signal(
                cell_states_signal()
                    .map(move |states| {
                        let items = states.get(&source_hold.name());
                        if let Some(list_items) = items.and_then(|v| v.as_list_items()) {
                            // Filter items by field value
                            let filtered: Vec<&Value> = list_items.iter().filter(|item| {
                                if let Value::Object(obj) = item {
                                    if let Some(field_val) = obj.get(filter_field.as_ref()) {
                                        // Evaluate the field value (might be a CellRef)
                                        let evaluated = evaluate_dd_value_for_filter(field_val, &states);
                                        let filter_eval = evaluate_dd_value_for_filter(&filter_value, &states);
                                        return evaluated == filter_eval;
                                    }
                                }
                                false
                            }).collect();
                            zoon::println!("[DD FilteredMappedListRef] filtered {} -> {} items from HOLD",
                                list_items.len(), filtered.len());
                            let children: Vec<RawElOrText> = filtered.iter().map(|item| {
                                let concrete_element = element_template.substitute_placeholder(item);
                                render_dd_value(&concrete_element)
                            }).collect();
                            Some(Column::new().s(Gap::new().y(gap)).items(children).unify())
                        } else {
                            zoon::println!("[DD FilteredMappedListRef] no items or wrong type in HOLD");
                            None
                        }
                    })
            )
            .unify();
    }

    // FilteredMappedListWithPredicate: Generic predicate filtering for stripe rendering
    // Detects hover patterns in the template and remaps to unique IDs per item
    if let Some(Value::FilteredMappedListWithPredicate { source_hold, predicate_template, element_template }) = items_value {
        zoon::println!("[DD render_stripe] FilteredMappedListWithPredicate: source_hold={}", source_hold);
        let source_hold = source_hold.clone();
        let predicate_template = predicate_template.clone();
        let element_template = element_template.clone();

        // Find original hover link/cell from template for remapping
        let original_hover_link = find_template_hover_link(&element_template);
        let original_hover_cell = find_template_hover_cell(&element_template);
        zoon::println!("[DD FilteredMappedListWithPredicate] Template hover detection: link={:?}, cell={:?}",
            original_hover_link, original_hover_cell);

        return El::new()
            .child_signal(
                cell_states_signal()
                    .map(move |states| {
                        let items = states.get(&source_hold.name());
                        // Get pre-instantiated elements from list_elements (these have unique hover IDs)
                        let list_elements = states.get("list_elements");
                        let elements_vec: Option<&std::sync::Arc<Vec<Value>>> = match list_elements {
                            Some(Value::List(elems)) => Some(elems),
                            _ => None,
                        };
                        zoon::println!("[DD FilteredMappedListWithPredicate] source_hold={}, items={:?}, list_elements={}",
                            source_hold, items.map(|v| dd_value_variant_name(v)),
                            elements_vec.map(|e| e.len()).unwrap_or(0));
                        if let Some(list_items) = items.and_then(|v| v.as_list_items()) {
                            zoon::println!("[DD FilteredMappedListWithPredicate] list has {} items", list_items.len());
                            // Filter items and their corresponding elements together
                            // Items and elements are parallel arrays (same order)
                            let filtered_with_idx: Vec<(usize, &Value)> = list_items.iter()
                                .enumerate()
                                .filter(|(_, item)| {
                                    let resolved_predicate = predicate_template.substitute_placeholder(item);
                                    let evaluated = evaluate_dd_value_for_filter(&resolved_predicate, &states);
                                    zoon::println!("[DD FilteredMappedListWithPredicate] predicate evaluated to: {:?} (truthy={})", evaluated, evaluated.is_truthy());
                                    evaluated.is_truthy()
                                })
                                .collect();
                            zoon::println!("[DD FilteredMappedListWithPredicate] filtered to {} items", filtered_with_idx.len());

                            // Calculate offset: list_elements only has dynamically added items
                            // Original items (idx < offset) use template, dynamic items use list_elements
                            let num_list_items = list_items.len();
                            let num_elements = elements_vec.map(|e| e.len()).unwrap_or(0);
                            let dynamic_offset = num_list_items.saturating_sub(num_elements);

                            // Render items with hover remapping
                            let children: Vec<RawElOrText> = filtered_with_idx.iter().map(|(idx, item)| {
                                // Generate unique hover IDs for this item if template has hover pattern
                                let concrete_element = if original_hover_link.is_some() && original_hover_cell.is_some() {
                                    // Use STABLE hover IDs based on source_hold name + item index
                                    // This ensures the same item gets the same hover ID across re-renders
                                    let new_hover_link = format!("filtered_hover_{}_{}", source_hold.name(), idx);
                                    let new_hover_cell = format!("hover_{}", new_hover_link);

                                    // Register HoverState action for this item
                                    // NOTE: Do NOT call update_cell_no_persist here - we're inside a signal callback!
                                    // Updating CELL_STATES inside cell_states_signal().map() causes infinite recursion.
                                    // The hover cell will be lazily initialized when the first hover event fires.
                                    add_dynamic_link_action(
                                        new_hover_link.clone(),
                                        DynamicLinkAction::HoverState(new_hover_cell.clone())
                                    );

                                    zoon::println!("[DD FilteredMappedListWithPredicate] Item {}: remapped hover {} -> {}, cell {} -> {}",
                                        idx, original_hover_link.as_ref().unwrap(), new_hover_link,
                                        original_hover_cell.as_ref().unwrap(), new_hover_cell);

                                    // Substitute with hover remapping
                                    element_template.substitute_placeholder_with_hover_remap(
                                        item,
                                        &new_hover_link,
                                        &new_hover_cell,
                                        original_hover_link.as_deref(),
                                        original_hover_cell.as_deref(),
                                    )
                                } else {
                                    zoon::println!("[DD FilteredMappedListWithPredicate] Using template for idx {} (no hover pattern)", idx);
                                    element_template.substitute_placeholder(item)
                                };
                                render_dd_value(&concrete_element)
                            }).collect();
                            Some(Column::new().s(Gap::new().y(gap)).items(children).unify())
                        } else {
                            zoon::println!("[DD FilteredMappedListWithPredicate] no list found or wrong type");
                            None
                        }
                    })
            )
            .unify();
    }

    let items: Vec<RawElOrText> = fields
        .get("items")
        .and_then(|v| match v {
            Value::List(items) => {
                zoon::println!("[DD render_stripe] iterating {} items", items.len());
                Some(items.iter().enumerate().map(|(idx, item)| {
                    zoon::println!("[DD render_stripe] item[{}] variant={}", idx, dd_value_variant_name(item));
                    render_dd_value(item)
                }).collect())
            }
            Value::Collection(handle) => {
                zoon::println!("[DD render_stripe] iterating {} collection items", handle.len());
                Some(handle.iter().enumerate().map(|(idx, item)| {
                    zoon::println!("[DD render_stripe] collection item[{}] variant={}", idx, dd_value_variant_name(item));
                    render_dd_value(item)
                }).collect())
            }
            _ => None,
        })
        .unwrap_or_default();

    // Extract style properties (like render_container does)
    let style = fields.get("style");

    // Width: Fill or exact value
    let width_fill = style
        .and_then(|s| s.get("width"))
        .map(|v| matches!(v, Value::Tagged { tag, .. } if tag.as_ref() == "Fill"))
        .unwrap_or(false);

    // Background color (Oklch)
    let bg_color = style
        .and_then(|s| s.get("background"))
        .and_then(|bg| bg.get("color"))
        .and_then(|c| dd_oklch_to_css(c));

    // Font size and color
    let font_size = style
        .and_then(|s| s.get("font"))
        .and_then(|f| f.get("size"))
        .and_then(|v| match v {
            Value::Number(n) => Some(n.0 as u32),
            _ => None,
        });
    let font_color = style
        .and_then(|s| s.get("font"))
        .and_then(|f| f.get("color"))
        .and_then(|c| dd_oklch_to_css(c));

    // Padding: row is y (vertical), column is x (horizontal) in Boon terminology
    let padding_y = style
        .and_then(|s| s.get("padding"))
        .and_then(|p| p.get("row"))
        .and_then(|v| match v {
            Value::Number(n) => Some(n.0 as u32),
            _ => None,
        });
    let padding_x = style
        .and_then(|s| s.get("padding"))
        .and_then(|p| p.get("column"))
        .and_then(|v| match v {
            Value::Number(n) => Some(n.0 as u32),
            _ => None,
        });

    if direction == "Row" {
        let mut row = Row::new()
            .s(Gap::new().x(gap))
            .items(items);

        // Apply styles
        if width_fill {
            row = row.s(zoon::Width::fill());
        }
        if let Some(color) = bg_color {
            row = row.s(zoon::Background::new().color(color));
        }
        // Apply font styling (size and/or color)
        if font_size.is_some() || font_color.is_some() {
            let mut font = zoon::Font::new();
            if let Some(size) = font_size {
                font = font.size(size);
            }
            if let Some(ref color) = font_color {
                font = font.color(color.clone());
            }
            row = row.s(font);
        }
        if padding_x.is_some() || padding_y.is_some() {
            let mut padding = zoon::Padding::new();
            if let Some(x) = padding_x {
                padding = padding.x(x);
            }
            if let Some(y) = padding_y {
                padding = padding.y(y);
            }
            row = row.s(padding);
        }

        // Add hovered handler if present
        if let Some(link_id) = hovered_link_id {
            row.on_hovered_change(move |is_hovered| {
                fire_global_link_with_bool(&link_id, is_hovered);
            })
            .unify()
        } else {
            row.unify()
        }
    } else {
        // Default to Column
        let mut column = Column::new()
            .s(Gap::new().y(gap))
            .items(items);

        // Apply styles
        if width_fill {
            column = column.s(zoon::Width::fill());
        }
        if let Some(color) = bg_color {
            column = column.s(zoon::Background::new().color(color));
        }
        // Apply font styling (size and/or color)
        if font_size.is_some() || font_color.is_some() {
            let mut font = zoon::Font::new();
            if let Some(size) = font_size {
                font = font.size(size);
            }
            if let Some(ref color) = font_color {
                font = font.color(color.clone());
            }
            column = column.s(font);
        }
        if padding_x.is_some() || padding_y.is_some() {
            let mut padding = zoon::Padding::new();
            if let Some(x) = padding_x {
                padding = padding.x(x);
            }
            if let Some(y) = padding_y {
                padding = padding.y(y);
            }
            column = column.s(padding);
        }

        // Add hovered handler if present
        if let Some(link_id) = hovered_link_id {
            column.on_hovered_change(move |is_hovered| {
                fire_global_link_with_bool(&link_id, is_hovered);
            })
            .unify()
        } else {
            column.unify()
        }
    }
}


/// Render a stack (layered elements).
fn render_stack(fields: &Arc<std::collections::BTreeMap<Arc<str>, Value>>) -> RawElOrText {
    let layers: Vec<RawElOrText> = fields
        .get("layers")
        .and_then(|v| match v {
            Value::List(items) => Some(items.iter().map(|item| render_dd_value(item)).collect()),
            Value::Collection(handle) => Some(handle.iter().map(|item| render_dd_value(item)).collect()),
            _ => None,
        })
        .unwrap_or_default();

    Stack::new()
        .layers(layers)
        .unify()
}

/// Render a container element.
fn render_container(fields: &Arc<std::collections::BTreeMap<Arc<str>, Value>>) -> RawElOrText {
    let child = fields.get("child").or_else(|| fields.get("element"));

    // Extract style properties
    let style = fields.get("style");

    // Get size (sets both width and height)
    let size_opt = style
        .and_then(|s| s.get("size"))
        .and_then(|v| match v {
            Value::Number(n) => Some(n.0 as u32),
            _ => None,
        });

    // Get background URL
    let bg_url_opt = style
        .and_then(|s| s.get("background"))
        .and_then(|bg| bg.get("url"))
        .and_then(|v| match v {
            Value::Text(s) => Some(s.to_string()),
            _ => None,
        });

    // Get font color value for checking if it's reactive
    let font_color_value = style
        .and_then(|s| s.get("font"))
        .and_then(|f| f.get("color"))
        .cloned();

    // Debug: log what font_color_value we got
    if font_color_value.is_some() {
        zoon::println!("[DD render_container] font_color_value: {:?}", font_color_value);
    }

    // Check if font color contains a WhileRef (needs reactive rendering)
    let is_reactive_font_color = font_color_value.as_ref().map_or(false, |c| {
        if let Value::Tagged { fields, .. } = c {
            // Check if any field (like lightness) is a WhileRef with computation
            let has_reactive = fields.values().any(|v| matches!(v, Value::WhileRef { computation: Some(_), .. }));
            zoon::println!("[DD render_container] is_reactive_font_color: {}", has_reactive);
            has_reactive
        } else {
            false
        }
    });

    // Get font size
    let font_size = style
        .and_then(|s| s.get("font"))
        .and_then(|f| f.get("size"))
        .and_then(|v| match v {
            Value::Number(n) => Some(n.0 as u32),
            _ => None,
        });

    // Build base element with styles (before adding child due to typestate)
    let base = El::new();
    let base = match size_opt {
        Some(size) => base.s(Width::exact(size)).s(Height::exact(size)),
        None => base,
    };
    let base = match bg_url_opt {
        Some(url) => base.s(Background::new().url(url)),
        None => base,
    };

    // Apply font styling - reactive if color contains WhileRef with computation
    let base = if is_reactive_font_color {
        // Reactive font color - need to watch holds
        let font_color_value = font_color_value.clone();
        base.s(Font::new().size(font_size.unwrap_or(14)).color_signal(
            cell_states_signal()
                .map(move |_states| {
                    // Re-evaluate color on any hold change
                    font_color_value.as_ref()
                        .and_then(|c| dd_oklch_to_css(c))
                        .unwrap_or_else(|| "inherit".to_string())
                })
                .dedupe_cloned()
        ))
    } else {
        // Static font styling
        let font_color_css = font_color_value.as_ref().and_then(|c| dd_oklch_to_css(c));
        if font_size.is_some() || font_color_css.is_some() {
            let mut font = Font::new();
            if let Some(size) = font_size {
                font = font.size(size);
            }
            if let Some(color) = font_color_css {
                font = font.color(color);
            }
            base.s(font)
        } else {
            base
        }
    };

    // Apply padding
    let base = {
        let padding_value = style.and_then(|s| s.get("padding"));

        // Check if padding is a single number (applies to all sides)
        let padding_all = padding_value.and_then(|p| match p {
            Value::Number(n) => Some(n.0 as u32),
            _ => None,
        });

        if let Some(all) = padding_all {
            // Single value applies to all sides
            base.s(Padding::all(all))
        } else {
            // Padding is an Object with specific values (row, column, left, right, top, bottom)
            let padding_obj = padding_value;

            // Get padding values (row = horizontal/x, column = vertical/y)
            let padding_row = padding_obj
                .and_then(|p| p.get("row"))
                .and_then(|v| match v {
                    Value::Number(n) => Some(n.0 as u32),
                    _ => None,
                });
            let padding_column = padding_obj
                .and_then(|p| p.get("column"))
                .and_then(|v| match v {
                    Value::Number(n) => Some(n.0 as u32),
                    _ => None,
                });
            let padding_left = padding_obj
                .and_then(|p| p.get("left"))
                .and_then(|v| match v {
                    Value::Number(n) => Some(n.0 as u32),
                    _ => None,
                });
            let padding_right = padding_obj
                .and_then(|p| p.get("right"))
                .and_then(|v| match v {
                    Value::Number(n) => Some(n.0 as u32),
                    _ => None,
                });
            let padding_top = padding_obj
                .and_then(|p| p.get("top"))
                .and_then(|v| match v {
                    Value::Number(n) => Some(n.0 as u32),
                    _ => None,
                });
            let padding_bottom = padding_obj
                .and_then(|p| p.get("bottom"))
                .and_then(|v| match v {
                    Value::Number(n) => Some(n.0 as u32),
                    _ => None,
                });

            if padding_row.is_some() || padding_column.is_some() || padding_left.is_some()
                || padding_right.is_some() || padding_top.is_some() || padding_bottom.is_some() {
                let mut padding = Padding::new();
                if let Some(x) = padding_row {
                    padding = padding.x(x);
                }
                if let Some(y) = padding_column {
                    padding = padding.y(y);
                }
                if let Some(left) = padding_left {
                    padding = padding.left(left);
                }
                if let Some(right) = padding_right {
                    padding = padding.right(right);
                }
                if let Some(top) = padding_top {
                    padding = padding.top(top);
                }
                if let Some(bottom) = padding_bottom {
                    padding = padding.bottom(bottom);
                }
                base.s(padding)
            } else {
                base
            }
        }
    };

    // Apply height
    let base = {
        let height_opt = style
            .and_then(|s| s.get("height"))
            .and_then(|v| match v {
                Value::Number(n) => Some(n.0 as u32),
                _ => None,
            });

        if let Some(height) = height_opt {
            base.s(Height::exact(height))
        } else {
            base
        }
    };

    // Apply transform (rotation)
    let base = {
        let rotate_opt = style
            .and_then(|s| s.get("transform"))
            .and_then(|t| t.get("rotate"))
            .and_then(|v| match v {
                Value::Number(n) => Some(n.0 as i32),
                _ => None,
            });

        if let Some(rotate) = rotate_opt {
            zoon::println!("[DD render_container] Applying transform rotate: {} degrees", rotate);
            base.s(Transform::new().rotate(rotate))
        } else {
            base
        }
    };

    // Add child (changes typestate, so must be last)
    match child {
        Some(c) => base.child(render_dd_value(c)).unify(),
        None => base.unify(),
    }
}

/// Render a text input element.
fn render_text_input(fields: &Arc<std::collections::BTreeMap<Arc<str>, Value>>) -> RawElOrText {
    // Placeholder can be a simple string or an object with a "text" field
    // e.g., placeholder: [text: TEXT { Type and press Enter... }]
    let placeholder = fields
        .get("placeholder")
        .map(|v| {
            // Try to get .text field from object, otherwise use to_display_string
            v.get("text")
                .map(|t| t.to_display_string())
                .unwrap_or_else(|| v.to_display_string())
        })
        .unwrap_or_default();

    let text_field = fields.get("text");
    zoon::println!("[DD TextInput] text field value: {:?}", text_field);

    let text = text_field
        .map(|v| {
            // If it's a CellRef, resolve to the actual stored value
            if let Value::CellRef(cell_id) = v {
                let resolved = get_cell_value(&cell_id.name())
                    .map(|hv| hv.to_display_string())
                    .unwrap_or_default();
                zoon::println!("[DD TextInput] CellRef({}) resolved to: '{}'", cell_id, resolved);
                resolved
            } else {
                let display = v.to_display_string();
                zoon::println!("[DD TextInput] Non-CellRef to_display_string: '{}'", display);
                display
            }
        })
        .unwrap_or_default();

    // Check for focus: True tag
    let should_focus = fields
        .get("focus")
        .map(|v| match v {
            Value::Tagged { tag, .. } => BoolTag::is_true(tag.as_ref()),
            Value::Bool(b) => *b,
            _ => false,
        })
        .unwrap_or(false);

    // Extract key_down LinkRef from element.event.key_down
    let key_down_link_id = fields
        .get("element")
        .and_then(|e| e.get("event"))
        .and_then(|e| e.get("key_down"))
        .and_then(|v| match v {
            Value::LinkRef(id) => Some(id.to_string()),
            _ => None,
        });

    // DEBUG: Log text_input rendering info
    let element_field = fields.get("element");
    let event_field = element_field.and_then(|e| e.get("event"));
    let key_down_field = event_field.and_then(|e| e.get("key_down"));
    zoon::println!("[DD TextInput] render_text_input: key_down_link_id={:?}, element={:?}, event={:?}, key_down={:?}, focus={}",
        key_down_link_id, element_field.is_some(), event_field.is_some(), key_down_field, should_focus);

    // Extract change LinkRef from element.event.change
    let change_link_id = fields
        .get("element")
        .and_then(|e| e.get("event"))
        .and_then(|e| e.get("change"))
        .and_then(|v| match v {
            Value::LinkRef(id) => Some(id.to_string()),
            _ => None,
        });

    // Extract blur LinkRef separately (for editing inputs)
    let blur_link_id = fields
        .get("element")
        .and_then(|e| e.get("event"))
        .and_then(|e| e.get("blur"))
        .and_then(|v| match v {
            Value::LinkRef(id) => Some(id.to_string()),
            _ => None,
        });

    // TextInput builder uses typestate, so we need separate code paths
    // for different combinations of event handlers
    match (key_down_link_id, change_link_id) {
        (Some(key_link), Some(change_link)) => {
            let do_focus = should_focus;

            // CRITICAL: Track text in a cell for ALL inputs with both key_down and change events.
            // For editing inputs (those with blur handlers), use blur_link_id for the cell name.
            // For regular inputs (like todo_mvc's main input), use change_link for the cell name.
            // This ensures we can read the text when Enter is pressed (DOM access fails with CDP).
            let text_cell = blur_link_id.as_ref()
                .map(|id| format!("editing_text_{}", id))
                .or_else(|| Some(format!("input_text_{}", change_link)));

            // Initialize text hold with current text if needed
            let current_text = if let Some(ref cell_id) = text_cell {
                let existing = super::super::io::get_cell_value(cell_id);
                zoon::println!("[DD TextInput] text_cell='{}', existing cell value={:?}", cell_id, existing);
                existing
                    .and_then(|v| if let super::super::core::value::Value::Text(t) = v { Some(t.to_string()) } else { None })
                    .unwrap_or_else(|| {
                        // First time - initialize with the original text
                        zoon::println!("[DD TextInput] Initializing text_cell='{}' with text='{}'", cell_id, text);
                        super::super::io::update_cell_no_persist(cell_id, super::super::core::value::Value::text(text.clone()));
                        text.clone()
                    })
            } else {
                text.clone()
            };
            zoon::println!("[DD TextInput] Final current_text='{}'", current_text);

            let text_cell_for_change = text_cell.clone();
            let text_cell_for_keydown = text_cell.clone();
            let is_editing_input = blur_link_id.is_some();
            let initial_text_for_insert = current_text.clone();

            let input = TextInput::new()
                .id("dd_text_input")
                .placeholder(Placeholder::new(placeholder))
                .text(current_text)  // Use tracked text for editing inputs
                .focus(should_focus)
                .update_raw_el(move |raw_el| {
                    let raw_el = raw_el.attr("autocomplete", "off");
                    if do_focus {
                        // For dynamically shown inputs (like editing input), we need to:
                        // 1. Set the input value after insert (Zoon's .text() may not work reliably for WhileRef)
                        // 2. Call focus() after insert
                        // 3. Defer with requestAnimationFrame to win the focus race
                        //    against the main input which also has focus=true
                        let text_for_insert = initial_text_for_insert.clone();
                        raw_el.after_insert(move |el| {
                            #[cfg(target_arch = "wasm32")]
                            {
                                use zoon::wasm_bindgen::closure::Closure;
                                use zoon::wasm_bindgen::JsCast;

                                // Set the input value immediately (critical for editing inputs)
                                if !text_for_insert.is_empty() {
                                    if let Some(input) = el.dyn_ref::<zoon::web_sys::HtmlInputElement>() {
                                        input.set_value(&text_for_insert);
                                        zoon::println!("[DD TextInput] Set input value via after_insert: '{}'", text_for_insert);
                                    }
                                }

                                // Use double requestAnimationFrame: first lets current render complete,
                                // second ensures we focus after the main input's focus has been processed
                                let el_clone = el.clone();
                                let inner_closure = Closure::once(move || {
                                    let _ = el_clone.focus();
                                });
                                let outer_closure = Closure::once(move || {
                                    if let Some(window) = zoon::web_sys::window() {
                                        let _ = window.request_animation_frame(inner_closure.as_ref().unchecked_ref());
                                    }
                                    inner_closure.forget();
                                });
                                if let Some(window) = zoon::web_sys::window() {
                                    let _ = window.request_animation_frame(outer_closure.as_ref().unchecked_ref());
                                }
                                outer_closure.forget();
                            }
                            #[cfg(not(target_arch = "wasm32"))]
                            {
                                let _ = el.focus();
                            }
                        })
                    } else {
                        raw_el
                    }
                })
                .on_key_down_event(move |event| {
                    zoon::println!("[DD on_key_down_event] INPUT fired! key_link={}, is_editing={}", key_link, is_editing_input);
                    let key_name = match event.key() {
                        Key::Enter => {
                            zoon::println!("[DD on_key_down_event] Enter pressed, is_editing={}", is_editing_input);
                            // For Enter key, capture the input's current text value
                            // Try cell first (set by on_change), then DOM fallback (needed for CDP testing)
                            #[cfg(target_arch = "wasm32")]
                            {
                                let mut input_text = String::new();

                                // Try reading from tracked cell first (set by on_change events)
                                if let Some(ref cell_id) = text_cell_for_keydown {
                                    input_text = super::super::io::get_cell_value(cell_id)
                                        .and_then(|v| if let super::super::core::value::Value::Text(t) = v { Some(t.to_string()) } else { None })
                                        .unwrap_or_default();
                                    zoon::println!("[DD on_key_down_event] Cell value: '{}'", input_text);

                                    // Only clear the hold for editing inputs (blur-based workflow)
                                    if is_editing_input && !input_text.is_empty() {
                                        super::super::io::clear_cell(cell_id);
                                    }
                                }

                                // If cell is empty, try DOM fallback (needed when CDP typing doesn't trigger on_change)
                                if input_text.is_empty() {
                                    input_text = get_dd_text_input_value();
                                    zoon::println!("[DD on_key_down_event] DOM fallback value: '{}'", input_text);
                                }

                                zoon::println!("[DD on_key_down_event] Enter text captured: '{}'", input_text);
                                // Send input text (not just "Enter") so ListAppend can use it
                                fire_global_key_down(&key_link, &format!("Enter:{}", input_text));
                            }
                            #[cfg(not(target_arch = "wasm32"))]
                            {
                                fire_global_key_down(&key_link, "Enter");
                            }
                            return;
                        }
                        Key::Escape => {
                            // For editing inputs, clear the tracked hold on Escape
                            if is_editing_input {
                                if let Some(ref cell_id) = text_cell_for_keydown {
                                    super::super::io::clear_cell(cell_id);
                                }
                            }
                            "Escape"
                        },
                        Key::Other(k) => k.as_str(),
                    };
                    // Send key name with the event so WHEN pattern matching works
                    fire_global_key_down(&key_link, key_name);
                })
                .on_change(move |new_text| {
                    // Track text changes in the cell (for all inputs with change event)
                    if let Some(ref cell_id) = text_cell_for_change {
                        super::super::io::update_cell_no_persist(cell_id, super::super::core::value::Value::text(new_text.clone()));
                    }
                    fire_global_link(&change_link);
                });
            // Add blur handler if blur_link_id is set (for editing inputs)
            if let Some(blur_link) = blur_link_id.clone() {
                // The grace period (set in SetTrue handler) protects against spurious blur events
                // during the focus race. We just need to call fire_global_blur - it will check
                // the grace period and ignore blur if still in grace period.
                input
                    .on_blur(move || {
                        fire_global_blur(&blur_link);
                    }).unify()
            } else {
                input.unify()
            }
        }
        (Some(key_link), None) => {
            let do_focus = should_focus;
            TextInput::new()
                .id("dd_text_input")
                .placeholder(Placeholder::new(placeholder))
                .text(text)
                .focus(should_focus)
                .update_raw_el(move |raw_el| {
                    let raw_el = raw_el.attr("autocomplete", "off");
                    if do_focus {
                        raw_el.after_insert(|el| {
                            #[cfg(target_arch = "wasm32")]
                            {
                                use zoon::wasm_bindgen::closure::Closure;
                                use zoon::wasm_bindgen::JsCast;
                                let el_clone = el.clone();
                                let closure = Closure::once(move || {
                                    let _ = el_clone.focus();
                                });
                                if let Some(window) = zoon::web_sys::window() {
                                    let _ = window.request_animation_frame(closure.as_ref().unchecked_ref());
                                }
                                closure.forget();
                            }
                            #[cfg(not(target_arch = "wasm32"))]
                            {
                                let _ = el.focus();
                            }
                        })
                    } else {
                        raw_el
                    }
                })
                .on_key_down_event(move |event| {
                    zoon::println!("[DD on_key_down_event] SIMPLE INPUT fired! key_link={}", key_link);
                    let key_name = match event.key() {
                        Key::Enter => {
                            zoon::println!("[DD on_key_down_event] Enter pressed in SIMPLE input");
                            #[cfg(target_arch = "wasm32")]
                            {
                                let input_text = get_dd_text_input_value();
                                fire_global_key_down(&key_link, &format!("Enter:{}", input_text));
                            }
                            #[cfg(not(target_arch = "wasm32"))]
                            {
                                fire_global_key_down(&key_link, "Enter");
                            }
                            return;
                        }
                        Key::Escape => "Escape",
                        Key::Other(k) => k.as_str(),
                    };
                    // Send key name with the event so WHEN pattern matching works
                    fire_global_key_down(&key_link, key_name);
                })
                .on_change(|_| {})
                .unify()
        }
        (None, Some(change_link)) => {
            let do_focus = should_focus;
            TextInput::new()
                .id("dd_text_input")
                .placeholder(Placeholder::new(placeholder))
                .text(text)
                .focus(should_focus)
                .update_raw_el(move |raw_el| {
                    let raw_el = raw_el.attr("autocomplete", "off");
                    if do_focus {
                        raw_el.after_insert(|el| {
                            #[cfg(target_arch = "wasm32")]
                            {
                                use zoon::wasm_bindgen::closure::Closure;
                                use zoon::wasm_bindgen::JsCast;
                                let el_clone = el.clone();
                                let closure = Closure::once(move || {
                                    let _ = el_clone.focus();
                                });
                                if let Some(window) = zoon::web_sys::window() {
                                    let _ = window.request_animation_frame(closure.as_ref().unchecked_ref());
                                }
                                closure.forget();
                            }
                            #[cfg(not(target_arch = "wasm32"))]
                            {
                                let _ = el.focus();
                            }
                        })
                    } else {
                        raw_el
                    }
                })
                .on_change(move |_text| {
                    fire_global_link(&change_link);
                })
                .unify()
        }
        (None, None) => {
            let do_focus = should_focus;
            TextInput::new()
                .id("dd_text_input")
                .placeholder(Placeholder::new(placeholder))
                .text(text)
                .focus(should_focus)
                .update_raw_el(move |raw_el| {
                    let raw_el = raw_el.attr("autocomplete", "off");
                    if do_focus {
                        raw_el.after_insert(|el| {
                            #[cfg(target_arch = "wasm32")]
                            {
                                use zoon::wasm_bindgen::closure::Closure;
                                use zoon::wasm_bindgen::JsCast;
                                let el_clone = el.clone();
                                let closure = Closure::once(move || {
                                    let _ = el_clone.focus();
                                });
                                if let Some(window) = zoon::web_sys::window() {
                                    let _ = window.request_animation_frame(closure.as_ref().unchecked_ref());
                                }
                                closure.forget();
                            }
                            #[cfg(not(target_arch = "wasm32"))]
                            {
                                let _ = el.focus();
                            }
                        })
                    } else {
                        raw_el
                    }
                })
                .on_change(|_| {})
                .unify()
        }
    }
}

/// Render a checkbox element.
// Default checkbox SVG icons (same as dynamic items for visual consistency)
const UNCHECKED_SVG: &str = "data:image/svg+xml;utf8,%3Csvg%20xmlns%3D%22http%3A//www.w3.org/2000/svg%22%20width%3D%2240%22%20height%3D%2240%22%20viewBox%3D%22-10%20-18%20100%20135%22%3E%3Ccircle%20cx%3D%2250%22%20cy%3D%2250%22%20r%3D%2250%22%20fill%3D%22none%22%20stroke%3D%22%23ededed%22%20stroke-width%3D%223%22/%3E%3C/svg%3E";
const CHECKED_SVG: &str = "data:image/svg+xml;utf8,%3Csvg%20xmlns%3D%22http%3A//www.w3.org/2000/svg%22%20width%3D%2240%22%20height%3D%2240%22%20viewBox%3D%22-10%20-18%20100%20135%22%3E%3Ccircle%20cx%3D%2250%22%20cy%3D%2250%22%20r%3D%2250%22%20fill%3D%22none%22%20stroke%3D%22%23bddad5%22%20stroke-width%3D%223%22/%3E%3Cpath%20fill%3D%22%235dc2af%22%20d%3D%22M72%2025L42%2071%2027%2056l-4%204%2020%2020%2034-52z%22/%3E%3C/svg%3E";

// DEAD CODE DELETED: render_default_checkbox_icon() - was never called

fn render_checkbox(fields: &Arc<std::collections::BTreeMap<Arc<str>, Value>>) -> RawElOrText {
    zoon::println!("[DD render_checkbox] CALLED with fields={:?}", fields.keys().collect::<Vec<_>>());
    // Extract checked value - can be Bool, Tagged, or CellRef (reactive)
    let checked_value = fields.get("checked").cloned();
    zoon::println!("[DD render_checkbox] checked_value={:?}", checked_value);

    // Extract click LinkRef from element.event.click if present
    let click_link_id = fields
        .get("element")
        .and_then(|e| e.get("event"))
        .and_then(|e| e.get("click"))
        .and_then(|v| match v {
            Value::LinkRef(id) => Some(id.to_string()),
            _ => None,
        });

    // Use Checkbox component for proper role="checkbox" accessibility
    // with custom 40x40 SVG icon for visual consistency

    // Check if checked is a CellRef (reactive checkbox)
    if let Some(Value::CellRef(cell_id)) = &checked_value {
        // Reactive checkbox - observe HOLD state changes
        let cell_id_for_signal = cell_id.to_string();
        let cell_id_for_icon = cell_id.to_string();

        let checkbox = Checkbox::new()
            .label_hidden("checkbox")
            .checked_signal(
                cell_states_signal()
                    .map({
                        let cell_id = cell_id_for_signal.clone();
                        move |states| {
                            states.get(&cell_id)
                                .map(|v| match v {
                                    Value::Bool(b) => *b,
                                    Value::Tagged { tag, .. } => BoolTag::is_true(tag.as_ref()),
                                    _ => false,
                                })
                                .unwrap_or(false)
                        }
                    })
            )
            .icon({
                // Observe HOLD state directly for icon - more reliable than checked_mutable
                // when elements are recreated during re-renders
                let cell_id_for_icon = cell_id.to_string();
                move |_checked_mutable| {
                    El::new()
                        .s(zoon::Width::exact(40))
                        .s(zoon::Height::exact(40))
                        .update_raw_el(|raw_el| raw_el.style("pointer-events", "none"))
                        .s(zoon::Background::new().url_signal(
                            cell_states_signal()
                                .map({
                                    let cell_id = cell_id_for_icon.clone();
                                    move |states| {
                                        let checked = states.get(&cell_id)
                                            .map(|v| match v {
                                                Value::Bool(b) => *b,
                                                Value::Tagged { tag, .. } => BoolTag::is_true(tag.as_ref()),
                                                _ => false,
                                            })
                                            .unwrap_or(false);
                                        if checked { CHECKED_SVG } else { UNCHECKED_SVG }
                                    }
                                })
                        ))
                }
            });

        // For reactive checkboxes with a CellRef, toggle the HOLD value directly
        // Fire the link event - DynamicLinkAction::BoolToggle will handle the actual toggle
        // NOTE: Don't call toggle_cell_bool directly here! That would cause a double toggle
        // because fire_global_link checks for DynamicLinkAction and executes BoolToggle.
        let cell_id_for_toggle = cell_id.to_string();
        if let Some(ref link_id) = click_link_id {
            zoon::println!("[DD render_checkbox] RETURNING reactive checkbox with link_id={}", link_id);
            let link_id_owned = link_id.clone();
            // Use raw DOM event listener to bypass potential Zoon event handling issues
            return checkbox
                .update_raw_el(move |raw_el| {
                    let link_id = link_id_owned.clone();
                    raw_el.event_handler(move |_: zoon::events::Click| {
                        zoon::println!("[DD CHECKBOX CLICK] RAW event handler invoked! link_id={}", link_id);
                        // Only fire link - DynamicLinkAction::BoolToggle handles the toggle
                        fire_global_link(&link_id);
                    })
                })
                .unify();
        } else {
            zoon::println!("[DD render_checkbox] RETURNING reactive checkbox WITHOUT link_id");
            // No link, just toggle the HOLD directly
            let cell_id_clone = cell_id_for_toggle.clone();
            return checkbox
                .update_raw_el(move |raw_el| {
                    let cell_id = cell_id_clone.clone();
                    raw_el.event_handler(move |_: zoon::events::Click| {
                        toggle_cell_bool(&cell_id);
                    })
                })
                .unify();
        }
    }

    // Check for ComputedRef (e.g., toggle all checkbox where checked = items_count == completed_items_count)
    if let Some(Value::ComputedRef { computation, source_hold }) = &checked_value {
        zoon::println!("[DD render_checkbox] ComputedRef checkbox with source_hold={}", source_hold);

        // Check if there's a custom icon - toggle all uses a "❯" character with reactive color
        let custom_icon = fields.get("icon").cloned();

        let computation = computation.clone();
        let source_hold = source_hold.clone();

        let checkbox = if let Some(icon_value) = custom_icon {
            zoon::println!("[DD render_checkbox] Using custom icon for ComputedRef checkbox");
            // Use custom icon with reactive rendering
            Checkbox::new()
                .label_hidden("checkbox")
                .checked_signal(
                    cell_states_signal()
                        .map({
                            let computation = computation.clone();
                            move |_states| {
                                // Evaluate the computation to get current checked state
                                use super::super::core::value::evaluate_computed;
                                let result = evaluate_computed(&computation, &Value::Unit);
                                match result {
                                    Value::Bool(b) => b,
                                    _ => false,
                                }
                            }
                        })
                )
                .icon(move |_checked_mutable| {
                    // Render the custom icon value - wrap with pointer-events: none so clicks propagate to checkbox
                    El::new()
                        .update_raw_el(|raw_el| raw_el.style("pointer-events", "none"))
                        .child(render_dd_value(&icon_value))
                })
        } else {
            // Use default SVG icon
            Checkbox::new()
                .label_hidden("checkbox")
                .checked_signal(
                    cell_states_signal()
                        .map({
                            let computation = computation.clone();
                            move |_states| {
                                use super::super::core::value::evaluate_computed;
                                let result = evaluate_computed(&computation, &Value::Unit);
                                match result {
                                    Value::Bool(b) => b,
                                    _ => false,
                                }
                            }
                        })
                )
                .icon({
                    let computation = computation.clone();
                    move |_checked_mutable| {
                        El::new()
                            .s(zoon::Width::exact(40))
                            .s(zoon::Height::exact(40))
                            .update_raw_el(|raw_el| raw_el.style("pointer-events", "none"))
                            .s(zoon::Background::new().url_signal(
                                cell_states_signal()
                                    .map({
                                        let computation = computation.clone();
                                        move |_states| {
                                            use super::super::core::value::evaluate_computed;
                                            let result = evaluate_computed(&computation, &Value::Unit);
                                            let checked = match result {
                                                Value::Bool(b) => b,
                                                _ => false,
                                            };
                                            if checked { CHECKED_SVG } else { UNCHECKED_SVG }
                                        }
                                    })
                            ))
                    }
                })
        };

        if let Some(link_id) = click_link_id {
            return checkbox
                .on_click(move || {
                    fire_global_link(&link_id);
                })
                .unify();
        } else {
            return checkbox.unify();
        }
    }

    // Static checkbox - extract checked state directly
    let checked = checked_value
        .as_ref()
        .map(|v| match v {
            Value::Bool(b) => *b,
            Value::Tagged { tag, .. } => BoolTag::is_true(tag.as_ref()),
            _ => false,
        })
        .unwrap_or(false);

    // Check for custom icon (static case)
    let custom_icon = fields.get("icon").cloned();

    // Build Checkbox with either custom or default icon
    let checkbox = if let Some(icon_value) = custom_icon {
        Checkbox::new()
            .label_hidden("checkbox")
            .checked(checked)
            .icon(move |_checked_mutable| {
                render_dd_value(&icon_value)
            })
    } else {
        let svg_url = if checked { CHECKED_SVG } else { UNCHECKED_SVG };
        Checkbox::new()
            .label_hidden("checkbox")
            .checked(checked)
            .icon(move |_checked_mutable| {
                // CRITICAL: Use pointer_events_none() so clicks pass through to checkbox parent
                El::new()
                    .s(zoon::Width::exact(40))
                    .s(zoon::Height::exact(40))
                    .s(zoon::Background::new().url(svg_url))
                    .update_raw_el(|raw_el| raw_el.style("pointer-events", "none"))  // Let clicks pass through
                    .unify()
            })
    };

    if let Some(link_id) = click_link_id {
        checkbox
            .on_click(move || {
                fire_global_link(&link_id);
            })
            .unify()
    } else {
        checkbox.unify()
    }
}

/// Render a label element.
fn render_label(fields: &Arc<std::collections::BTreeMap<Arc<str>, Value>>) -> RawElOrText {
    let label_value = fields
        .get("label")
        .or_else(|| fields.get("text"));

    // Extract double_click LinkRef from element.event.double_click if present
    let double_click_link_id = fields
        .get("element")
        .and_then(|e| e.get("event"))
        .and_then(|e| e.get("double_click"))
        .and_then(|v| match v {
            Value::LinkRef(id) => Some(id.to_string()),
            _ => None,
        });

    // Extract font styling from style.font
    let style = fields.get("style");
    let font_color_css = style
        .and_then(|s| s.get("font"))
        .and_then(|f| f.get("color"))
        .and_then(|c| dd_oklch_to_css(c));
    let font_size = style
        .and_then(|s| s.get("font"))
        .and_then(|f| f.get("size"))
        .and_then(|v| if let Value::Number(n) = v { Some(n.0 as u32) } else { None });

    // Extract strikethrough from style.font.line.strikethrough (can be CellRef for reactive)
    let strikethrough_hold = style
        .and_then(|s| s.get("font"))
        .and_then(|f| f.get("line"))
        .and_then(|l| l.get("strikethrough"))
        .and_then(|v| match v {
            Value::CellRef(id) => Some(id.to_string()),
            _ => None,
        });

    let label = match label_value {
        Some(Value::CellRef(name)) => {
            // Reactive label - update when HOLD state changes
            let cell_id = name.to_string();
            Label::new()
                .label_signal(
                    cell_states_signal()
                        .map(move |states| {
                            states
                                .get(&cell_id)
                                .map(|v| v.to_display_string())
                                .unwrap_or_default()
                        })
                )
                .for_input("dd_text_input")
        }
        Some(Value::ReactiveText { parts }) => {
            // Reactive TEXT with interpolated values - evaluated at render time
            let parts = parts.clone();
            Label::new()
                .label_signal(
                    cell_states_signal()
                        .map(move |_states| {
                            // Evaluate all parts with current HOLD state
                            parts.iter()
                                .map(|part| part.to_display_string())
                                .collect::<String>()
                        })
                )
                .for_input("dd_text_input")
        }
        Some(v) => {
            // Static label
            Label::new()
                .label(v.to_display_string())
                .for_input("dd_text_input")
        }
        None => {
            Label::new()
                .label("")
                .for_input("dd_text_input")
        }
    };

    // Build font style
    let mut font = Font::new();
    if let Some(color) = font_color_css {
        font = font.color(color);
    }
    if let Some(size) = font_size {
        font = font.size(size);
    }

    // If strikethrough is tied to a CellRef (reactive completed state), we need a signal
    // For now, apply static styling and let the parent handle reactive strikethrough
    let label_with_style = label.s(font);

    // Add double_click handler if present
    if let Some(link_id) = double_click_link_id {
        label_with_style
            .on_double_click(move || {
                fire_global_link(&link_id);
            })
            .unify()
    } else {
        label_with_style.unify()
    }
}

/// Render a paragraph element.
fn render_paragraph(fields: &Arc<std::collections::BTreeMap<Arc<str>, Value>>) -> RawElOrText {
    // Try "contents" first (plural - used by Element/paragraph), then fallback to "content" or "text"
    let content = if let Some(items) = fields.get("contents").and_then(|v| v.as_list_items()) {
        // Render list items and join their text representations
        items.iter()
            .map(|item| extract_text_content(item))
            .collect::<Vec<_>>()
            .join("")
    } else {
        fields
            .get("content")
            .or_else(|| fields.get("text"))
            .map(|v| extract_text_content(v))
            .unwrap_or_default()
    };

    Paragraph::new()
        .content(content)
        .unify()
}

/// Extract display text from a Value, handling nested elements like links.
fn extract_text_content(value: &Value) -> String {
    match value {
        Value::Text(s) => s.to_string(),
        Value::Unit => " ".to_string(), // Text/space() renders as Unit
        Value::Tagged { tag, fields } if ElementTag::is_element(tag.as_ref()) => {
            // For Element tags, check the element type and extract appropriate text
            let element_type = fields
                .get("_element_type")
                .and_then(|v| match v {
                    Value::Text(s) => Some(s.as_ref()),
                    _ => None,
                });

            match element_type {
                Some("link") => {
                    // Extract label from link element
                    fields.get("label")
                        .map(|v| extract_text_content(v))
                        .unwrap_or_default()
                }
                Some("paragraph") => {
                    // Recursively extract from paragraph contents
                    if let Some(items) = fields.get("contents").and_then(|v| v.as_list_items()) {
                        items.iter()
                            .map(|item| extract_text_content(item))
                            .collect::<Vec<_>>()
                            .join("")
                    } else {
                        String::new()
                    }
                }
                _ => {
                    // For other elements, try to extract from label or child
                    fields.get("label")
                        .or_else(|| fields.get("child"))
                        .map(|v| extract_text_content(v))
                        .unwrap_or_default()
                }
            }
        }
        _ => value.to_display_string(),
    }
}

/// Render a link element.
fn render_link(fields: &Arc<std::collections::BTreeMap<Arc<str>, Value>>) -> RawElOrText {
    let label = fields
        .get("label")
        .map(|v| v.to_display_string())
        .unwrap_or_default();

    let to = fields
        .get("to")
        .map(|v| v.to_display_string())
        .unwrap_or_else(|| "#".to_string());

    Link::new()
        .label(label)
        .to(to)
        .unify()
}
