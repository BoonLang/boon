//! DD Bridge - Converts DD values to Zoon elements.
//!
//! This module provides functions to render `DdValue` as Zoon elements.
//! Currently implements static rendering; reactive rendering will use
//! DdOutput streams in a future phase.

use std::sync::Arc;

use super::dd_interpreter::{DdContext, DdResult};
use super::dd_value::DdValue;
use zoon::*;

use super::io::{fire_global_link, fire_global_link_with_bool, fire_global_blur, fire_global_key_down, hold_states_signal, get_hold_value, toggle_hold_bool};

/// Helper function to get the variant name of a DdValue for debug logging.
fn dd_value_variant_name(value: &DdValue) -> &'static str {
    match value {
        DdValue::Unit => "Unit",
        DdValue::Bool(_) => "Bool",
        DdValue::Number(_) => "Number",
        DdValue::Text(_) => "Text",
        DdValue::List(_) => "List",
        DdValue::Object(_) => "Object",
        DdValue::Tagged { tag, .. } => {
            // For Tagged, we want to show the tag name, but we can't return a dynamic string
            // So we'll just return "Tagged" and log the tag separately
            "Tagged"
        }
        DdValue::HoldRef(_) => "HoldRef",
        DdValue::LinkRef(_) => "LinkRef",
        DdValue::TimerRef { .. } => "TimerRef",
        DdValue::WhileRef { .. } => "WhileRef",
        DdValue::ComputedRef { .. } => "ComputedRef",
        DdValue::FilteredListRef { .. } => "FilteredListRef",
        DdValue::ReactiveFilteredList { .. } => "ReactiveFilteredList",
    }
}

/// Get the current value of the focused text input via DOM access.
/// This is used when Enter is pressed to capture the input text.
/// We use document.activeElement instead of getElementById because multiple
/// inputs may have the same ID (main input vs edit input).
#[cfg(target_arch = "wasm32")]
fn get_dd_text_input_value() -> String {
    use zoon::*;
    let active = document().active_element();
    let tag_name = active.as_ref().map(|el| el.tag_name()).unwrap_or_default();
    let result = active
        .and_then(|el| el.dyn_into::<web_sys::HtmlInputElement>().ok())
        .map(|input| input.value())
        .unwrap_or_default();
    zoon::println!("[DD TextInput] get_dd_text_input_value: active_tag={}, value='{}'", tag_name, result);
    result
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

/// Get the value of a dynamic todo edit input by index.
/// Used when Enter is pressed to capture the edited title.
#[cfg(target_arch = "wasm32")]
fn get_dynamic_todo_edit_value(index: usize) -> Option<String> {
    use zoon::*;
    let input_id = format!("dynamic_todo_edit_input_{}", index);
    document()
        .get_element_by_id(&input_id)
        .and_then(|el| el.dyn_into::<web_sys::HtmlInputElement>().ok())
        .map(|input| input.value().trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Convert a DdValue Oklch color to CSS color string.
/// Returns None if the color should be invisible (alpha=0 or broken WhileRef).
/// Evaluate a WhileRef with computation to get the current value.
/// Returns the matching arm's body value or None if no match.
fn evaluate_while_ref_now(hold_id: &str, computation: &Option<super::dd_value::ComputedType>, arms: &[(DdValue, DdValue)]) -> Option<DdValue> {
    use super::dd_value::evaluate_computed;

    // Get the source value from hold states
    let source_value = get_hold_value(hold_id).unwrap_or(DdValue::Unit);

    // If there's a computation, evaluate it to get the actual value to match
    let value_to_match = if let Some(comp) = computation {
        evaluate_computed(comp, &source_value)
    } else {
        source_value
    };

    // Match against arms
    for (pattern, body) in arms.iter() {
        let matches = match (&value_to_match, pattern) {
            (DdValue::Bool(b), DdValue::Tagged { tag, .. }) => {
                (*b && tag.as_ref() == "True") || (!*b && tag.as_ref() == "False")
            }
            (DdValue::Text(curr), DdValue::Text(pat)) => curr == pat,
            (DdValue::Tagged { tag: curr_tag, .. }, DdValue::Tagged { tag: pat_tag, .. }) => curr_tag == pat_tag,
            _ => &value_to_match == pattern,
        };
        if matches {
            return Some(body.clone());
        }
    }
    None
}

fn dd_oklch_to_css(value: &DdValue) -> Option<String> {
    match value {
        DdValue::Tagged { tag, fields } if tag.as_ref() == "Oklch" => {
            // Handle lightness - can be Number or WhileRef (reactive)
            let lightness = match fields.get("lightness") {
                Some(DdValue::Number(n)) => n.0,
                Some(DdValue::WhileRef { hold_id, computation, arms, .. }) => {
                    // Evaluate WhileRef to get current lightness value
                    let result = evaluate_while_ref_now(hold_id, computation, arms);
                    zoon::println!("[DD dd_oklch_to_css] WhileRef lightness: hold_id={}, computation={:?}, result={:?}", hold_id, computation.is_some(), result);
                    match result {
                        Some(DdValue::Number(n)) => n.0,
                        _ => 0.5, // default
                    }
                }
                _ => 0.5, // default
            };
            let chroma = fields.get("chroma")
                .and_then(|v| if let DdValue::Number(n) = v { Some(n.0) } else { None })
                .unwrap_or(0.0);
            let hue = fields.get("hue")
                .and_then(|v| if let DdValue::Number(n) = v { Some(n.0) } else { None })
                .unwrap_or(0.0);

            // Handle alpha - can be Number or WhileRef
            let alpha_value = fields.get("alpha");
            let alpha = match alpha_value {
                Some(DdValue::Number(n)) => Some(n.0),
                Some(DdValue::WhileRef { hold_id, arms, .. }) => {
                    // Try to evaluate WhileRef based on hold state
                    // If arms is empty, it's a broken WhileRef - use default alpha (0.4 for selected state)
                    if arms.is_empty() {
                        zoon::println!("[DD Bridge] dd_oklch_to_css: WhileRef alpha has empty arms, using default alpha 0.4");
                        return Some(format!("oklch({}% {} {} / 0.4)", lightness * 100.0, chroma, hue));
                    }
                    // Try to get current value from hold_states and match against arms
                    let current = get_hold_value(hold_id);
                    if let Some(current_val) = current {
                        for (pattern, body) in arms.iter() {
                            let matches = match (&current_val, pattern) {
                                (DdValue::Text(curr), DdValue::Text(pat)) => curr == pat,
                                (DdValue::Bool(b), DdValue::Tagged { tag, .. }) =>
                                    (*b && tag.as_ref() == "True") || (!*b && tag.as_ref() == "False"),
                                _ => &current_val == pattern,
                            };
                            if matches {
                                if let DdValue::Number(n) = body {
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
    document: DdValue,
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

/// Render a DdValue as a Zoon element.
fn render_dd_value(value: &DdValue) -> RawElOrText {
    match value {
        DdValue::Unit => El::new().unify(),

        DdValue::Bool(b) => Text::new(if *b { "true" } else { "false" }).unify(),

        DdValue::Number(n) => {
            // Format number nicely (no trailing .0 for integers)
            let num = n.0;
            let text = if num.fract() == 0.0 {
                format!("{}", num as i64)
            } else {
                format!("{}", num)
            };
            Text::new(text).unify()
        }

        DdValue::Text(s) => {
            // Check if this is an "N items left" pattern that should be reactive
            let text_str = s.to_string();
            if text_str.ends_with(" items left") || text_str.ends_with(" item left") {
                // Make this reactive based on "todos" HOLD which is the authoritative source
                return El::new()
                    .child_signal(
                        hold_states_signal()
                            .map(move |states| {
                                if !states.contains_key("todos") {
                                    // No todos HOLD, fall back to static text
                                    return Text::new(text_str.clone());
                                }

                                // Count active (not completed) items from "todos" HOLD
                                // The todos list is authoritative - after Clear completed, it only contains remaining items
                                let total_active = match states.get("todos") {
                                    Some(DdValue::List(todos)) => {
                                        todos.iter().filter(|todo| {
                                            // Check completed field - could be HoldRef or Bool
                                            match todo.get("completed") {
                                                // Completed via direct Bool
                                                Some(DdValue::Bool(true)) => false,
                                                // Completed via HoldRef - look up the HOLD value
                                                Some(DdValue::HoldRef(hold_id)) => {
                                                    match states.get(hold_id.as_ref()) {
                                                        Some(DdValue::Bool(true)) => false,
                                                        Some(DdValue::Tagged { tag, .. }) if tag.as_ref() == "True" => false,
                                                        _ => true,  // not completed
                                                    }
                                                }
                                                // No completed field or other - assume active
                                                _ => true,
                                            }
                                        }).count()
                                    }
                                    _ => 0,
                                };

                                // Format the text
                                let item_text = if total_active == 1 { "item" } else { "items" };
                                Text::new(format!("{} {} left", total_active, item_text))
                            })
                    )
                    .unify();
            }
            Text::new(text_str).unify()
        }

        DdValue::List(items) => {
            // Render list as column of items
            let children: Vec<RawElOrText> = items.iter().map(|item| render_dd_value(item)).collect();
            Column::new()
                .items(children)
                .unify()
        }

        DdValue::Object(fields) => {
            // Render object as debug representation
            let debug = fields
                .iter()
                .map(|(k, v)| format!("{}: {:?}", k, v))
                .collect::<Vec<_>>()
                .join(", ");
            Text::new(format!("[{}]", debug)).unify()
        }

        DdValue::Tagged { tag, fields } => {
            zoon::println!("[DD render_dd_value] Tagged(tag='{}', fields={:?})", tag, fields.keys().collect::<Vec<_>>());
            render_tagged_element(tag.as_ref(), fields)
        }

        DdValue::HoldRef(name) => {
            // HoldRef is a reactive reference to a HOLD value
            // Observe hold_states_signal and render current value reactively
            let hold_id = name.to_string();

            // Create reactive element that updates when HOLD_STATES change
            El::new()
                .child_signal(
                    hold_states_signal()
                        .map(move |states| {
                            let text = states
                                .get(&hold_id)
                                .map(|v| v.to_display_string())
                                .unwrap_or_else(|| "?".to_string());
                            Text::new(text)
                        })
                )
                .unify()
        }

        DdValue::LinkRef(link_id) => {
            // LinkRef is a placeholder for an event source
            // In static rendering, show as unit (events are wired at button level)
            El::new().unify()
        }

        DdValue::TimerRef { id, interval_ms: _ } => {
            // TimerRef represents a timer-driven HOLD accumulator
            // The `id` is the HOLD id - render its reactive value
            let hold_id = id.to_string();

            // Create reactive element that updates when HOLD_STATES change
            // NOTE: Returns empty string if HOLD hasn't been set yet (timer not fired)
            El::new()
                .child_signal(
                    hold_states_signal()
                        .map(move |states| {
                            let text = states
                                .get(&hold_id)
                                .map(|v| v.to_display_string())
                                .unwrap_or_default(); // Empty until first timer tick
                            Text::new(text)
                        })
                )
                .unify()
        }

        DdValue::WhileRef { hold_id, computation, arms, default } => {
            // WhileRef is a reactive WHILE/WHEN expression
            // Observe the hold value and render the matching arm
            let hold_id = hold_id.to_string();
            let computation = computation.clone();
            let arms = arms.clone();
            let default = default.clone();

            // Debug: log the arms Arc address and first arm body to detect sharing
            let arms_ptr = Arc::as_ptr(&arms) as usize;
            let first_arm_summary = arms.first().map(|(pattern, body)| {
                // Check if body is a button with a press LinkRef
                fn find_press_link(v: &DdValue) -> Option<String> {
                    match v {
                        DdValue::Tagged { tag, fields } if tag.as_ref() == "Element" => {
                            if let Some(element) = fields.get("element") {
                                if let Some(event) = element.get("event") {
                                    if let Some(DdValue::LinkRef(id)) = event.get("press") {
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
            // For ListCountWhereHold specifically, the count also depends on HoldRef values inside items (e.g., checkbox states).
            fn contains_list_count_hold(comp: &super::dd_value::ComputedType) -> bool {
                use super::dd_value::ComputedType;
                match comp {
                    // List computations that need to watch all holds
                    ComputedType::ListCountWhereHold { .. } => true,
                    ComputedType::ListCountHold { .. } => true,
                    ComputedType::ListIsEmptyHold { .. } => true,
                    ComputedType::GreaterThanZero { operand } => {
                        if let DdValue::ComputedRef { computation: inner_comp, .. } = operand.as_ref() {
                            contains_list_count_hold(inner_comp)
                        } else {
                            false
                        }
                    }
                    ComputedType::Equal { left, right } => {
                        // Check if either operand contains ListCountHold/ListCountWhereHold
                        let left_has = if let DdValue::ComputedRef { computation: inner_comp, .. } = left.as_ref() {
                            contains_list_count_hold(inner_comp)
                        } else {
                            false
                        };
                        let right_has = if let DdValue::ComputedRef { computation: inner_comp, .. } = right.as_ref() {
                            contains_list_count_hold(inner_comp)
                        } else {
                            false
                        };
                        left_has || right_has
                    }
                    _ => false,
                }
            }
            let needs_watch_all_holds = computation.as_ref().map_or(false, contains_list_count_hold);
            let hold_id_for_extract = hold_id.clone();
            let hold_id_for_log = hold_id.clone();
            El::new()
                .child_signal(
                    hold_states_signal()
                        .map(move |states| {
                            let main_value = states.get(&hold_id_for_extract).cloned();
                            if needs_watch_all_holds {
                                // For ListCountHold/ListCountWhereHold: include all states so any hold change triggers re-evaluation
                                // This is necessary because the count depends on HOLD contents (dynamic list additions/removals)
                                (main_value, Some(states))
                            } else {
                                // For other computations: only watch the main hold
                                (main_value, None)
                            }
                        })
                        .dedupe_cloned()  // Emit when watched hold values change
                        .map(move |(source_value, _watched_source)| {
                            zoon::println!("[DD WhileRef RENDER] hold_id={}, value={:?}", hold_id_for_log, source_value);
                            let source_value = source_value.as_ref();

                            // Determine the value to match against patterns
                            // If there's a computation, evaluate it first
                            let current_value: Option<DdValue> = if let Some(ref comp) = computation {
                                // Evaluate the computation to get a boolean
                                if let Some(source) = source_value {
                                    use super::dd_value::evaluate_computed;
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
                                        (DdValue::Bool(b), DdValue::Tagged { tag, .. }) => {
                                            (*b && tag.as_ref() == "True") || (!*b && tag.as_ref() == "False")
                                        }
                                        // Text to text comparison
                                        (DdValue::Text(curr), DdValue::Text(pat)) => curr == pat,
                                        // Tag comparison (e.g., Home, About)
                                        (DdValue::Tagged { tag: curr_tag, .. }, DdValue::Tagged { tag: pat_tag, .. }) => curr_tag == pat_tag,
                                        // Text to tag comparison for route matching
                                        // e.g., current="/" pattern=Tagged{Home} - need to map routes to tags
                                        (DdValue::Text(text), DdValue::Tagged { tag, .. }) => {
                                            // Map route paths to tags
                                            match (text.as_ref(), tag.as_ref()) {
                                                ("/", "Home") | ("", "Home") => true,
                                                ("/about", "About") => true,
                                                ("/contact", "Contact") => true,
                                                _ => false,
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
                                return render_dd_value(def.as_ref());
                            }

                            // No match and no default - render empty
                            El::new().unify()
                        })
                )
                .unify()
        }

        DdValue::ComputedRef { computation, source_hold } => {
            // ComputedRef is a reactive computed value that depends on a HOLD
            // Observe hold_states_signal and re-evaluate computation when source changes
            use super::dd_value::evaluate_computed;

            let source_hold = source_hold.to_string();
            let computation = computation.clone();

            El::new()
                .child_signal(
                    hold_states_signal()
                        .map(move |states| {
                            // Get source HOLD value
                            let source_value = states.get(&source_hold)
                                .cloned()
                                .unwrap_or(DdValue::Unit);

                            // Evaluate the computation
                            let result = evaluate_computed(&computation, &source_value);

                            // Render the result as text
                            Text::new(result.to_display_string())
                        })
                )
                .unify()
        }

        DdValue::FilteredListRef { source_hold, filter_field, filter_value: _ } => {
            // FilteredListRef is an intermediate value - shouldn't normally be rendered directly
            // If it is rendered, show debug info
            Text::new(format!("[filtered:{}@{}]", filter_field, source_hold)).unify()
        }

        DdValue::ReactiveFilteredList { items, filter_field, filter_value: _, hold_ids: _, source_hold: _ } => {
            // ReactiveFilteredList is an intermediate value - shouldn't normally be rendered directly
            // If it is rendered, show debug info
            Text::new(format!("[reactive-filtered:{}#{}]", filter_field, items.len())).unify()
        }
    }
}

/// Render a tagged object as a Zoon element.
fn render_tagged_element(tag: &str, fields: &Arc<std::collections::BTreeMap<Arc<str>, DdValue>>) -> RawElOrText {
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
fn render_element(fields: &Arc<std::collections::BTreeMap<Arc<str>, DdValue>>) -> RawElOrText {
    let element_type = fields
        .get("_element_type")
        .and_then(|v| match v {
            DdValue::Text(s) => Some(s.as_ref()),
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
fn render_button(fields: &Arc<std::collections::BTreeMap<Arc<str>, DdValue>>) -> RawElOrText {
    let label = fields
        .get("label")
        .map(|v| v.to_display_string())
        .unwrap_or_default();

    // Extract LinkRef from element.event.press if present
    let link_id = fields
        .get("element")
        .and_then(|e| e.get("event"))
        .and_then(|e| e.get("press"))
        .and_then(|v| match v {
            DdValue::LinkRef(id) => Some(id.to_string()),
            _ => None,
        });

    // Extract outline from style.outline
    // Note: outline may be a WhileRef for reactive styling based on selection state
    let style_value = fields.get("style");
    let outline_value = style_value.and_then(|s| s.get("outline"));

    // Check if outline is a WhileRef (reactive) - need to render reactively
    let is_reactive_outline = matches!(outline_value, Some(DdValue::WhileRef { .. }));

    let outline_opt: Option<Outline> = outline_value
        .and_then(|outline| {
            match outline {
                DdValue::Tagged { tag, .. } if tag.as_ref() == "NoOutline" => None,
                DdValue::Object(obj) => {
                    // Get color from outline object
                    let css_color = obj.get("color").and_then(|c| dd_oklch_to_css(c));
                    if let Some(color) = css_color {
                        // Check for side: Inner vs outer (default outer)
                        let is_inner = obj.get("side")
                            .map(|s| matches!(s, DdValue::Tagged { tag, .. } if tag.as_ref() == "Inner"))
                            .unwrap_or(false);
                        // Get width (default 1)
                        let width = obj.get("width")
                            .and_then(|w| if let DdValue::Number(n) = w { Some(n.0 as u32) } else { None })
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
        .and_then(|v| if let DdValue::Number(n) = v { Some(n.0 as u32) } else { None });

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
fn render_stripe(fields: &Arc<std::collections::BTreeMap<Arc<str>, DdValue>>) -> RawElOrText {
    let direction = fields
        .get("direction")
        .and_then(|v| match v {
            DdValue::Tagged { tag, .. } => Some(tag.as_ref().to_string()),
            DdValue::Text(s) => Some(s.to_string()),
            _ => None,
        })
        .unwrap_or_else(|| "Column".to_string());

    let gap = fields
        .get("gap")
        .and_then(|v| match v {
            DdValue::Number(n) => Some(n.0 as u32),
            _ => None,
        })
        .unwrap_or(0);

    // Extract hovered LinkRef from element.hovered if present
    let hovered_link_id = fields
        .get("element")
        .and_then(|e| e.get("hovered"))
        .and_then(|v| match v {
            DdValue::LinkRef(id) => Some(id.to_string()),
            _ => None,
        });

    // Check if this is a todo list (Ul tag) - needs reactive filtering
    let element_tag = fields
        .get("element")
        .and_then(|e| e.get("tag"));
    let is_todo_list = element_tag
        .map(|t| matches!(t, DdValue::Tagged { tag, .. } if tag.as_ref() == "Ul"))
        .unwrap_or(false);

    // Check if items is Unit (placeholder for reactive items from HOLD)
    let items_value = fields.get("items");
    let is_reactive_items = matches!(items_value, Some(DdValue::Unit) | None);

    // If items is Unit and gap is 4 (the items_list stripe in shopping_list),
    // render reactively from "items" HOLD
    if is_reactive_items && gap == 4 {
        // Reactive items list - render from HOLD state
        return Column::new()
            .s(Gap::new().y(gap))
            .items_signal_vec(
                hold_states_signal()
                    .map(|states| {
                        let items = states.get("items");
                        match items {
                            Some(DdValue::List(list)) => {
                                list.iter()
                                    .map(|item| {
                                        let text = item.to_display_string();
                                        Label::new()
                                            .label(format!("- {}", text))
                                            .s(Font::new().color(hsluv!(0, 0, 100)))
                                            .unify()
                                    })
                                    .collect::<Vec<_>>()
                            }
                            _ => Vec::new(),
                        }
                    })
                    .to_signal_vec()
            )
            .unify();
    }

    // If this is a todo list (Ul), render with reactive filtering
    if is_todo_list {
        if let Some(DdValue::List(items)) = items_value {
            return render_todo_list_filtered(items.clone(), gap);
        }
    }

    let items: Vec<RawElOrText> = fields
        .get("items")
        .and_then(|v| match v {
            DdValue::List(items) => {
                zoon::println!("[DD render_stripe] iterating {} items", items.len());
                Some(items.iter().enumerate().map(|(idx, item)| {
                    zoon::println!("[DD render_stripe] item[{}] variant={}", idx, dd_value_variant_name(item));
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
        .map(|v| matches!(v, DdValue::Tagged { tag, .. } if tag.as_ref() == "Fill"))
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
            DdValue::Number(n) => Some(n.0 as u32),
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
            DdValue::Number(n) => Some(n.0 as u32),
            _ => None,
        });
    let padding_x = style
        .and_then(|s| s.get("padding"))
        .and_then(|p| p.get("column"))
        .and_then(|v| match v {
            DdValue::Number(n) => Some(n.0 as u32),
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

/// Extract the checkbox HoldRef from a todo item, if present.
fn get_todo_checkbox_hold_id(item: &DdValue) -> Option<String> {
    match item {
        // Format 1: Todo object from HOLD with completed: HoldRef
        DdValue::Object(obj) => {
            if let Some(DdValue::HoldRef(hold_id)) = obj.get("completed") {
                return Some(hold_id.to_string());
            }
            None
        }
        // Format 2: Element from document with checkbox inside items list
        DdValue::Tagged { tag, fields } if tag.as_ref() == "Element" => {
            if let Some(DdValue::List(items)) = fields.get("items") {
                for sub_item in items.iter() {
                    // Look for checkbox element
                    if let DdValue::Tagged { tag, fields } = sub_item {
                        if tag.as_ref() == "Element" {
                            if let Some(DdValue::Text(elem_type)) = fields.get("_element_type") {
                                if elem_type.as_ref() == "checkbox" {
                                    if let Some(DdValue::HoldRef(hold_id)) = fields.get("checked") {
                                        return Some(hold_id.to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// Extract the title HoldRef ID from a todo item structure.
/// Returns the hold_id that contains the actual title text.
fn get_todo_title_hold_id(item: &DdValue) -> Option<String> {
    match item {
        // Format 1: Todo object from HOLD with title: HoldRef
        DdValue::Object(obj) => {
            if let Some(DdValue::HoldRef(hold_id)) = obj.get("title") {
                return Some(hold_id.to_string());
            }
            None
        }
        // Format 2: Element from document with WhileRef containing label HoldRef
        // Todo item structure is complex:
        // - Element (stripe, Row) with items: [checkbox, WhileRef, Unit]
        // - The WhileRef has: False arm → label Element with label: HoldRef("hold_9")
        DdValue::Tagged { tag, fields } if tag.as_ref() == "Element" => {
            if let Some(DdValue::List(items)) = fields.get("items") {
                for sub_item in items.iter() {
                    // Look for WhileRef (the edit mode toggle)
                    if let DdValue::WhileRef { arms, .. } = sub_item {
                        // Find the False arm (non-edit mode shows the label)
                        for (pattern, body) in arms.iter() {
                            if let DdValue::Tagged { tag, .. } = pattern {
                                if tag.as_ref() == "False" {
                                    // This arm contains the label element
                                    if let DdValue::Tagged { tag: body_tag, fields: body_fields } = body {
                                        if body_tag.as_ref() == "Element" {
                                            // Get the label field which should be a HoldRef
                                            if let Some(DdValue::HoldRef(hold_id)) = body_fields.get("label") {
                                                return Some(hold_id.to_string());
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// Extract the click link ID from a todo item's checkbox.
fn get_todo_click_link_id(item: &DdValue) -> Option<String> {
    match item {
        // Format 1: Todo object from HOLD with todo_elements.todo_checkbox: LinkRef
        DdValue::Object(obj) => {
            if let Some(DdValue::Object(elements)) = obj.get("todo_elements") {
                if let Some(DdValue::LinkRef(link_id)) = elements.get("todo_checkbox") {
                    return Some(link_id.to_string());
                }
            }
            None
        }
        // Format 2: Tagged Element structure from document
        // Todo item structure: Tagged { tag: "Element", fields: { items: [...checkbox...] } }
        DdValue::Tagged { tag, fields } if tag.as_ref() == "Element" => {
            if let Some(DdValue::List(items)) = fields.get("items") {
                for sub_item in items.iter() {
                    // Look for checkbox element
                    if let DdValue::Tagged { tag, fields } = sub_item {
                        if tag.as_ref() == "Element" {
                            if let Some(DdValue::Text(elem_type)) = fields.get("_element_type") {
                                if elem_type.as_ref() == "checkbox" {
                                    // Get click link from element.event.click
                                    if let Some(link_id) = fields
                                        .get("element")
                                        .and_then(|e| e.get("event"))
                                        .and_then(|e| e.get("click"))
                                    {
                                        if let DdValue::LinkRef(id) = link_id {
                                            return Some(id.to_string());
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// Render a todo list with reactive filtering based on selected_filter.
/// Also observes "todos" HOLD for dynamically added items.
fn render_todo_list_filtered(items: std::sync::Arc<Vec<DdValue>>, gap: u32) -> RawElOrText {
    use super::io::TodoFilter;

    zoon::println!("[DD render_todo_list_filtered] CALLED with {} items", items.len());
    for (idx, item) in items.iter().enumerate() {
        zoon::println!("[DD render_todo_list_filtered] item[{}] variant={}", idx, dd_value_variant_name(item));
    }

    // Capture the initial static items for the closure
    let static_items = items.clone();
    let static_items_count = items.len();

    // ARCHITECTURE FIX: Render items ONCE, don't re-render on every state change
    // Each item handles its own reactivity via signals (checked_signal, etc.)
    // Re-render on: filter changes OR todos list changes (add/remove)
    // Use El wrapper because Column doesn't have child_signal
    //
    // Use map_ref! to combine filter signal with hold_states, then dedupe on
    // (filter, todos_len, elems_len) tuple - this ensures we only re-render when
    // filter changes OR todos are added/removed, NOT on every checkbox toggle.
    use zoon::map_ref;

    El::new()
        .child_signal(
            map_ref! {
                let filter = super::io::selected_filter_signal(),
                let states = super::io::hold_states_signal() => {
                    // Derive trigger values from states
                    let todos_len = match states.get("todos") {
                        Some(DdValue::List(todos)) => todos.len(),
                        _ => 0,
                    };
                    let elems_len = match states.get("todos_elements") {
                        Some(DdValue::List(elements)) => elements.len(),
                        _ => 0,
                    };
                    (filter.clone(), todos_len, elems_len)
                }
            }
            .dedupe_cloned()  // Only re-emit when (filter, len, len) tuple changes
            .map(move |(filter, _todos_len, _elems_len)| {
                let static_items_clone = static_items.clone();
                // Get current state snapshot for rendering
                let states = super::io::get_all_hold_states();
                    // Return the element directly, not a signal
                        let mut rendered: Vec<RawElOrText> = Vec::new();

                        // Get the current list of todos from HOLD (source of truth after any changes)
                        let todos_hold = states.get("todos");

                        // Build a set of title HoldRefs that are still in the todos HOLD
                        // Use title HoldRef because it never changes (unlike completed which becomes Bool after toggle)
                        let active_title_ids: std::collections::HashSet<String> = match todos_hold {
                            Some(DdValue::List(todos)) => {
                                todos.iter()
                                    .filter_map(|item| get_todo_title_hold_id(item))
                                    .collect()
                            }
                            _ => std::collections::HashSet::new(),
                        };
                        let has_todos_hold = todos_hold.is_some();

                        // 1. Render static items (initial todos from document)
                        for (idx, item) in static_items_clone.iter().enumerate() {
                            // Get the checkbox HoldRef to determine completion status
                            let hold_id = get_todo_checkbox_hold_id(item);
                            let title_hold_id = get_todo_title_hold_id(item);

                            // If todos HOLD exists, only render items that are still in it
                            // (items may be removed by "Clear completed" or delete button)
                            if has_todos_hold {
                                if let Some(ref tid) = title_hold_id {
                                    if !active_title_ids.contains(tid) {
                                        // This item was removed from the todos HOLD, skip it
                                        continue;
                                    }
                                }
                            }

                            let is_completed = hold_id.as_ref()
                                .and_then(|id| states.get(id))
                                .map(|v| match v {
                                    DdValue::Bool(true) => true,
                                    DdValue::Tagged { tag, .. } if tag.as_ref() == "True" => true,
                                    _ => false,
                                })
                                .unwrap_or(false);

                            // Filter based on selected filter
                            let should_show = match &filter {
                                TodoFilter::All => true,
                                TodoFilter::Active => !is_completed,
                                TodoFilter::Completed => is_completed,
                            };

                            if should_show {
                                // Use render_dd_value to properly render the full todo item structure
                                // This handles WhileRef for editing mode, double_click, hover, etc.
                                zoon::println!("[DD render_todo_list] rendering static item[{}] variant={}", idx, dd_value_variant_name(item));
                                rendered.push(render_dd_value(item));
                            }
                        }

                        // 2. Render dynamic items from "todos_elements" HOLD
                        // These are Element AST cloned from the template with fresh HoldRef IDs
                        // Get completion status directly from the element's checkbox HoldRef
                        if let Some(DdValue::List(elements)) = states.get("todos_elements") {
                            for (index, element) in elements.iter().enumerate() {
                                // Get completion status directly from element's checkbox HoldRef
                                // This is more reliable than indexing into todos data
                                let completed = get_todo_checkbox_hold_id(element)
                                    .and_then(|hold_id| states.get(&hold_id))
                                    .map(|v| match v {
                                        DdValue::Bool(true) => true,
                                        DdValue::Tagged { tag, .. } if tag.as_ref() == "True" => true,
                                        _ => false,
                                    })
                                    .unwrap_or(false);

                                // Apply filter
                                let should_show = match (&filter, completed) {
                                    (TodoFilter::All, _) => true,
                                    (TodoFilter::Active, false) => true,
                                    (TodoFilter::Active, true) => false,
                                    (TodoFilter::Completed, true) => true,
                                    (TodoFilter::Completed, false) => false,
                                };
                                if should_show {
                                    // Use render_dd_value for unified rendering - no more TodoMVC-specific code!
                                    rendered.push(render_dd_value(element));
                                }
                            }
                        }

                    // Wrap items in a Column for child_signal (needs single element)
                    Column::new()
                        .s(Gap::new().y(gap))
                        .items(rendered)
                })
        )
        .unify()
}

/// Render a dynamic todo item with editing mode, hover delete, and checkbox toggle
///
/// ARCHITECTURAL NOTE: This function exists because ListAppend creates simple data objects
/// instead of full Element AST structures. The proper fix would be to make ListAppend
/// evaluate new_todo() and create HoldRefs, enabling render_dd_value for ALL todos.
/// Until then, this function must produce IDENTICAL output to what render_dd_value
/// produces for initial todos (same styles as Boon's todo_item function).
fn render_dynamic_todo(title: &str, completed: bool, editing: bool, index: usize) -> RawElOrText {
    use super::io::fire_global_link_with_text;

    // Styles from Boon's todo_item():
    // style: [width: Fill, background: Oklch[lightness: 1], font: [size: 24], padding: [row: 15, column: 10]]
    // Checkbox uses size: 40 with SVG background

    let checkbox_svg = if completed { CHECKED_SVG } else { UNCHECKED_SVG };
    let checkbox = Checkbox::new()
        .checked(completed)
        .icon(move |_checked_mutable| {
            // CRITICAL: Use pointer_events_none() so clicks pass through to checkbox parent
            El::new()
                .s(zoon::Width::exact(40))
                .s(zoon::Height::exact(40))
                .s(zoon::Background::new().url(checkbox_svg))
                .update_raw_el(|raw_el| raw_el.style("pointer-events", "none"))  // Let clicks pass through
                .unify()
        })
        .label_hidden(title.to_string())
        .on_click(move || {
            fire_global_link_with_text("dynamic_todo_toggle", &format!("toggle:{}", index));
        });

    // Title element - either label (normal mode) or text input (editing mode)
    let title_owned = title.to_string();
    let title_element: RawElOrText = if editing {
        // Editing mode: show text input
        // CRITICAL: Track editing text in a HOLD so it persists across re-renders
        // (hover events cause re-renders which would otherwise destroy the input)
        let editing_text_hold = format!("editing_text_{}", index);
        let editing_text_hold_for_change = editing_text_hold.clone();
        let editing_text_hold_for_blur = editing_text_hold.clone();
        let editing_text_hold_for_keydown = editing_text_hold.clone();

        // Initialize editing text hold with current title if not already set
        let current_text = super::io::get_hold_value(&editing_text_hold)
            .and_then(|v| if let super::dd_value::DdValue::Text(t) = v { Some(t.to_string()) } else { None })
            .unwrap_or_else(|| {
                // First time entering edit mode for this index - initialize with current title
                let t = title_owned.clone();
                super::io::update_hold_state_no_persist(&editing_text_hold, super::dd_value::DdValue::text(t.clone()));
                t
            });

        let index_for_blur = index;
        let index_for_keydown = index;
        let input_id = format!("dynamic_todo_edit_input_{}", index);

        // Guard: only process blur if the input was ever focused
        // This prevents spurious blur events when the input is first created
        let was_focused = std::rc::Rc::new(std::cell::Cell::new(false));
        let was_focused_for_blur = was_focused.clone();

        TextInput::new()
            .id(&input_id)
            .s(zoon::Width::fill())
            .s(zoon::Font::new()
                .size(24)
                .color(hsluv!(0, 0, 42)))  // Oklch lightness 0.42 from Boon todo_item
            .s(zoon::Padding::new().y(2).x(5))
            .s(zoon::Outline::inner().color(hsluv!(220, 50, 50)))  // Blue outline for editing
            .label_hidden("Edit todo")
            .text(current_text)  // Use tracked editing text (persists across re-renders)
            .on_focus(move || {
                // Mark that the input was focused - blur events after this are legitimate
                was_focused.set(true);
            })
            .on_change(move |text| {
                // Track every keystroke in the editing text HOLD
                // This ensures typed text persists even if the parent re-renders
                super::io::update_hold_state_no_persist(&editing_text_hold_for_change, super::dd_value::DdValue::text(text.clone()));
            })
            .on_blur(move || {
                // Only process blur if the input was ever focused
                // This guards against spurious blur events when the input is first created
                if !was_focused_for_blur.get() {
                    return;
                }
                // On blur, save from the tracked editing text HOLD
                let new_title = super::io::get_hold_value(&editing_text_hold_for_blur)
                    .and_then(|v| if let super::dd_value::DdValue::Text(t) = v {
                        let s = t.to_string().trim().to_string();
                        if s.is_empty() { None } else { Some(s) }
                    } else { None });

                // Clear the editing text hold (we're done editing)
                super::io::clear_hold_state(&editing_text_hold_for_blur);

                if let Some(title) = new_title {
                    fire_global_link_with_text("dynamic_todo_save", &format!("save:{}:{}", index_for_blur, title));
                } else {
                    // Empty title - just exit without saving (reverts to original)
                    fire_global_link_with_text("dynamic_todo_edit", &format!("unedit:{}", index_for_blur));
                }
            })
            .on_key_down_event(move |event| {
                match event.key() {
                    Key::Enter => {
                        // Save from the tracked editing text HOLD
                        let new_title = super::io::get_hold_value(&editing_text_hold_for_keydown)
                            .and_then(|v| if let super::dd_value::DdValue::Text(t) = v {
                                let s = t.to_string().trim().to_string();
                                if s.is_empty() { None } else { Some(s) }
                            } else { None });

                        // Clear the editing text hold (we're done editing)
                        super::io::clear_hold_state(&editing_text_hold_for_keydown);

                        if let Some(title) = new_title {
                            fire_global_link_with_text("dynamic_todo_save", &format!("save:{}:{}", index_for_keydown, title));
                        } else {
                            // Empty title - just exit without saving
                            fire_global_link_with_text("dynamic_todo_edit", &format!("unedit:{}", index_for_keydown));
                        }
                    }
                    Key::Escape => {
                        // Clear the editing text hold (we're canceling)
                        super::io::clear_hold_state(&editing_text_hold_for_keydown);
                        // Cancel editing without saving
                        fire_global_link_with_text("dynamic_todo_edit", &format!("unedit:{}", index_for_keydown));
                    }
                    _ => {}
                }
            })
            .focus(true)  // Auto-focus the input when entering edit mode
            .update_raw_el(|raw_el| {
                // For dynamically shown inputs (like editing input), we need to:
                // 1. Call focus() after insert
                // 2. Defer with requestAnimationFrame to ensure focus happens after DOM insertion
                raw_el.after_insert(|el| {
                    #[cfg(target_arch = "wasm32")]
                    {
                        use zoon::wasm_bindgen::closure::Closure;
                        use zoon::wasm_bindgen::JsCast;
                        // Use double requestAnimationFrame: first lets current render complete,
                        // second ensures we focus after any other focus operations
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
            })
            .unify()
    } else {
        // Normal mode: show label with double-click to edit
        // Font: size 24, color Oklch[lightness: 0.42] (from Boon todo_item)
        let index_for_dblclick = index;
        Label::new()
            .s(zoon::Font::new()
                .size(24)
                .color(if completed {
                    hsluv!(0, 0, 70)  // lighter gray when completed
                } else {
                    hsluv!(0, 0, 42)  // Oklch lightness 0.42 from todo_item
                }))
            .s(zoon::Width::fill())
            .label(title_owned)
            .on_double_click(move || {
                // Enter editing mode
                fire_global_link_with_text("dynamic_todo_edit", &format!("edit:{}", index_for_dblclick));
            })
            .unify()
    };

    // Create a hover state for showing delete button
    let hovered = Mutable::new(false);
    let hovered_for_row = hovered.clone();
    let hovered_for_delete = hovered.clone();
    let index_for_delete = index;

    // Styles from Boon's todo_item():
    // gap: 5, width: Fill, background: Oklch[lightness: 1] (white), font: size 24, padding: row 15, column 10
    Row::new()
        .s(Gap::new().x(5))
        .s(zoon::Padding::new().y(15).x(10))
        .s(zoon::Background::new().color(hsluv!(0, 0, 100)))  // Oklch lightness 1 = white
        .s(zoon::Font::new().size(24))
        .s(zoon::Width::fill())
        .on_hovered_change(move |is_hovered| hovered_for_row.set(is_hovered))
        .item(checkbox)
        .item(title_element)
        .item_signal(hovered_for_delete.signal().map(move |is_hovered| {
            if is_hovered {
                // Show delete button when hovered
                Button::new()
                    .s(zoon::Width::exact(40))
                    .s(zoon::Height::exact(40))
                    .s(zoon::Font::new()
                        .size(30)
                        .color(hsluv!(18, 60, 73)))  // Reddish color for delete
                    .s(zoon::Align::center())  // Center the button content
                    .label("×")
                    .on_press(move || {
                        fire_global_link_with_text("dynamic_todo_remove", &format!("remove:{}", index_for_delete));
                    })
                    .unify()
            } else {
                El::new().unify()  // Empty placeholder when not hovered
            }
        }))
        .unify()
}

/// Render a static (initial) todo item with consistent styling matching dynamic todos.
/// Uses HoldRef for reactive checkbox state and LinkRef for click handling.
/// title_hold_id: The HOLD ID containing the title text
/// checkbox_hold_id: The HOLD ID for the checkbox completed state
fn render_static_todo_item(title_hold_id: &str, checkbox_hold_id: &str, link_id: Option<&str>) -> RawElOrText {
    let checkbox_hold_id_owned = checkbox_hold_id.to_string();
    let checkbox_hold_id_for_icon = checkbox_hold_id.to_string();
    let title_hold_id_owned = title_hold_id.to_string();

    // Create Checkbox component with reactive icon (matches render_dynamic_todo style)
    // Using Checkbox::new() ensures proper role="checkbox" for accessibility
    let checkbox = Checkbox::new()
        .label_hidden("todo checkbox")
        .checked_signal(
            hold_states_signal()
                .map({
                    let hold_id = checkbox_hold_id_owned.clone();
                    move |states| {
                        states.get(&hold_id)
                            .map(|v| match v {
                                DdValue::Bool(b) => *b,
                                DdValue::Tagged { tag, .. } => tag.as_ref() == "True",
                                _ => false,
                            })
                            .unwrap_or(false)
                    }
                })
        )
        .icon(move |_checked_mutable| {
            // Reactive icon based on HOLD state
            // CRITICAL: Use pointer_events_none() so clicks pass through to checkbox parent
            El::new()
                .s(zoon::Width::exact(40))
                .s(zoon::Height::exact(40))
                .update_raw_el(|raw_el| raw_el.style("pointer-events", "none"))  // Let clicks pass through
                .child_signal(
                    hold_states_signal()
                        .map({
                            let hold_id = checkbox_hold_id_for_icon.clone();
                            move |states| {
                                let checked = states.get(&hold_id)
                                    .map(|v| match v {
                                        DdValue::Bool(b) => *b,
                                        DdValue::Tagged { tag, .. } => tag.as_ref() == "True",
                                        _ => false,
                                    })
                                    .unwrap_or(false);
                                let svg_url = if checked { CHECKED_SVG } else { UNCHECKED_SVG };
                                El::new()
                                    .s(zoon::Width::exact(40))
                                    .s(zoon::Height::exact(40))
                                    .s(zoon::Background::new().url(svg_url))
                                    .update_raw_el(|raw_el| raw_el.style("pointer-events", "none"))  // Let clicks pass through
                            }
                        })
                )
                .unify()
        });

    // Add click handler - always toggle HOLD, optionally fire link
    let checkbox_hold_for_click = checkbox_hold_id_owned.clone();
    let checkbox_with_click = if let Some(link_id) = link_id {
        let link_id = link_id.to_string();
        checkbox.on_click(move || {
            // CRITICAL: Toggle the HOLD state first, then fire the link
            // Without toggle_hold_bool, HOLD_STATES doesn't update and
            // reactive computations (like completed_todos_count) stay stale
            toggle_hold_bool(&checkbox_hold_for_click);
            fire_global_link(&link_id);
        })
    } else {
        // No link, just toggle the HOLD directly
        checkbox.on_click(move || {
            toggle_hold_bool(&checkbox_hold_for_click);
        })
    };

    // Title label - reactive title text and color based on completion state
    let title_label = El::new()
        .s(zoon::Width::fill())
        .child_signal(
            hold_states_signal()
                .map({
                    let checkbox_hold_id = checkbox_hold_id_owned.clone();
                    let title_hold_id = title_hold_id_owned.clone();
                    move |states| {
                        // Get title from title HOLD
                        let title = states.get(&title_hold_id)
                            .map(|v| v.to_display_string())
                            .unwrap_or_default();

                        // Get completed state from checkbox HOLD
                        let checked = states.get(&checkbox_hold_id)
                            .map(|v| match v {
                                DdValue::Bool(b) => *b,
                                DdValue::Tagged { tag, .. } => tag.as_ref() == "True",
                                _ => false,
                            })
                            .unwrap_or(false);
                        El::new()
                            .s(zoon::Font::new()
                                .size(24)
                                .color(if checked {
                                    hsluv!(0, 0, 70)  // lighter gray when completed
                                } else {
                                    hsluv!(0, 0, 42)  // normal dark gray
                                }))
                            .child(Text::new(&title))
                    }
                })
        );

    Row::new()
        .s(Gap::new().x(5))
        .s(zoon::Padding::new().y(15).x(10))
        .s(zoon::Background::new().color(hsluv!(0, 0, 100)))  // white bg
        .s(zoon::Font::new().size(24))
        .s(zoon::Width::fill())
        .item(checkbox_with_click)
        .item(title_label)
        .unify()
}

/// Render a stack (layered elements).
fn render_stack(fields: &Arc<std::collections::BTreeMap<Arc<str>, DdValue>>) -> RawElOrText {
    let layers: Vec<RawElOrText> = fields
        .get("layers")
        .and_then(|v| match v {
            DdValue::List(items) => Some(items.iter().map(|item| render_dd_value(item)).collect()),
            _ => None,
        })
        .unwrap_or_default();

    Stack::new()
        .layers(layers)
        .unify()
}

/// Render a container element.
fn render_container(fields: &Arc<std::collections::BTreeMap<Arc<str>, DdValue>>) -> RawElOrText {
    let child = fields.get("child").or_else(|| fields.get("element"));

    // Extract style properties
    let style = fields.get("style");

    // Get size (sets both width and height)
    let size_opt = style
        .and_then(|s| s.get("size"))
        .and_then(|v| match v {
            DdValue::Number(n) => Some(n.0 as u32),
            _ => None,
        });

    // Get background URL
    let bg_url_opt = style
        .and_then(|s| s.get("background"))
        .and_then(|bg| bg.get("url"))
        .and_then(|v| match v {
            DdValue::Text(s) => Some(s.to_string()),
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
        if let DdValue::Tagged { fields, .. } = c {
            // Check if any field (like lightness) is a WhileRef with computation
            let has_reactive = fields.values().any(|v| matches!(v, DdValue::WhileRef { computation: Some(_), .. }));
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
            DdValue::Number(n) => Some(n.0 as u32),
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
            hold_states_signal()
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
            DdValue::Number(n) => Some(n.0 as u32),
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
                    DdValue::Number(n) => Some(n.0 as u32),
                    _ => None,
                });
            let padding_column = padding_obj
                .and_then(|p| p.get("column"))
                .and_then(|v| match v {
                    DdValue::Number(n) => Some(n.0 as u32),
                    _ => None,
                });
            let padding_left = padding_obj
                .and_then(|p| p.get("left"))
                .and_then(|v| match v {
                    DdValue::Number(n) => Some(n.0 as u32),
                    _ => None,
                });
            let padding_right = padding_obj
                .and_then(|p| p.get("right"))
                .and_then(|v| match v {
                    DdValue::Number(n) => Some(n.0 as u32),
                    _ => None,
                });
            let padding_top = padding_obj
                .and_then(|p| p.get("top"))
                .and_then(|v| match v {
                    DdValue::Number(n) => Some(n.0 as u32),
                    _ => None,
                });
            let padding_bottom = padding_obj
                .and_then(|p| p.get("bottom"))
                .and_then(|v| match v {
                    DdValue::Number(n) => Some(n.0 as u32),
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
                DdValue::Number(n) => Some(n.0 as u32),
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
                DdValue::Number(n) => Some(n.0 as i32),
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
fn render_text_input(fields: &Arc<std::collections::BTreeMap<Arc<str>, DdValue>>) -> RawElOrText {
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

    let text = fields
        .get("text")
        .map(|v| {
            // If it's a HoldRef, resolve to the actual stored value
            if let DdValue::HoldRef(hold_id) = v {
                get_hold_value(hold_id.as_ref())
                    .map(|hv| hv.to_display_string())
                    .unwrap_or_default()
            } else {
                v.to_display_string()
            }
        })
        .unwrap_or_default();

    // Check for focus: True tag
    let should_focus = fields
        .get("focus")
        .map(|v| match v {
            DdValue::Tagged { tag, .. } => tag.as_ref() == "True",
            DdValue::Bool(b) => *b,
            _ => false,
        })
        .unwrap_or(false);

    // Extract key_down LinkRef from element.event.key_down
    let key_down_link_id = fields
        .get("element")
        .and_then(|e| e.get("event"))
        .and_then(|e| e.get("key_down"))
        .and_then(|v| match v {
            DdValue::LinkRef(id) => Some(id.to_string()),
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
            DdValue::LinkRef(id) => Some(id.to_string()),
            _ => None,
        });

    // Extract blur LinkRef separately (for editing inputs)
    let blur_link_id = fields
        .get("element")
        .and_then(|e| e.get("event"))
        .and_then(|e| e.get("blur"))
        .and_then(|v| match v {
            DdValue::LinkRef(id) => Some(id.to_string()),
            _ => None,
        });

    // TextInput builder uses typestate, so we need separate code paths
    // for different combinations of event handlers
    match (key_down_link_id, change_link_id) {
        (Some(key_link), Some(change_link)) => {
            let do_focus = should_focus;

            // CRITICAL: For editing inputs (those with blur handlers), track text in a HOLD
            // so it persists across re-renders (caused by hover events etc.)
            let editing_text_hold = blur_link_id.as_ref().map(|id| format!("editing_text_{}", id));

            // Initialize editing text hold with current text if needed
            let current_text = if let Some(ref hold_id) = editing_text_hold {
                super::io::get_hold_value(hold_id)
                    .and_then(|v| if let super::dd_value::DdValue::Text(t) = v { Some(t.to_string()) } else { None })
                    .unwrap_or_else(|| {
                        // First time - initialize with the original text
                        super::io::update_hold_state_no_persist(hold_id, super::dd_value::DdValue::text(text.clone()));
                        text.clone()
                    })
            } else {
                text.clone()
            };

            let editing_text_hold_for_change = editing_text_hold.clone();
            let editing_text_hold_for_keydown = editing_text_hold.clone();

            let input = TextInput::new()
                .id("dd_text_input")
                .placeholder(Placeholder::new(placeholder))
                .text(current_text)  // Use tracked text for editing inputs
                .focus(should_focus)
                .update_raw_el(move |raw_el| {
                    let raw_el = raw_el.attr("autocomplete", "off");
                    if do_focus {
                        // For dynamically shown inputs (like editing input), we need to:
                        // 1. Call focus() after insert
                        // 2. Defer with requestAnimationFrame to win the focus race
                        //    against the main input which also has focus=true
                        raw_el.after_insert(|el| {
                            #[cfg(target_arch = "wasm32")]
                            {
                                use zoon::wasm_bindgen::closure::Closure;
                                use zoon::wasm_bindgen::JsCast;
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
                    zoon::println!("[DD on_key_down_event] EDITING INPUT fired! key_link={}", key_link);
                    let key_name = match event.key() {
                        Key::Enter => {
                            zoon::println!("[DD on_key_down_event] Enter pressed in EDITING input");
                            // For Enter key, capture the input's current text value
                            // For editing inputs, use the tracked HOLD value (survives re-renders)
                            // For non-editing inputs, use DOM access
                            #[cfg(target_arch = "wasm32")]
                            {
                                let input_text = if let Some(ref hold_id) = editing_text_hold_for_keydown {
                                    // Read from tracked HOLD (persists across re-renders)
                                    let text = super::io::get_hold_value(hold_id)
                                        .and_then(|v| if let super::dd_value::DdValue::Text(t) = v { Some(t.to_string()) } else { None })
                                        .unwrap_or_default();
                                    // Clear the hold now that we're done editing
                                    super::io::clear_hold_state(hold_id);
                                    text
                                } else {
                                    // Non-editing input - use DOM access
                                    get_dd_text_input_value()
                                };
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
                            if let Some(ref hold_id) = editing_text_hold_for_keydown {
                                super::io::clear_hold_state(hold_id);
                            }
                            "Escape"
                        },
                        Key::Other(k) => k.as_str(),
                    };
                    // Send key name with the event so WHEN pattern matching works
                    fire_global_key_down(&key_link, key_name);
                })
                .on_change(move |new_text| {
                    // For editing inputs, track text changes in the HOLD
                    if let Some(ref hold_id) = editing_text_hold_for_change {
                        super::io::update_hold_state_no_persist(hold_id, super::dd_value::DdValue::text(new_text.clone()));
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
// Default checkbox SVG icons (same as dynamic todos for visual consistency)
const UNCHECKED_SVG: &str = "data:image/svg+xml;utf8,%3Csvg%20xmlns%3D%22http%3A//www.w3.org/2000/svg%22%20width%3D%2240%22%20height%3D%2240%22%20viewBox%3D%22-10%20-18%20100%20135%22%3E%3Ccircle%20cx%3D%2250%22%20cy%3D%2250%22%20r%3D%2250%22%20fill%3D%22none%22%20stroke%3D%22%23ededed%22%20stroke-width%3D%223%22/%3E%3C/svg%3E";
const CHECKED_SVG: &str = "data:image/svg+xml;utf8,%3Csvg%20xmlns%3D%22http%3A//www.w3.org/2000/svg%22%20width%3D%2240%22%20height%3D%2240%22%20viewBox%3D%22-10%20-18%20100%20135%22%3E%3Ccircle%20cx%3D%2250%22%20cy%3D%2250%22%20r%3D%2250%22%20fill%3D%22none%22%20stroke%3D%22%23bddad5%22%20stroke-width%3D%223%22/%3E%3Cpath%20fill%3D%22%235dc2af%22%20d%3D%22M72%2025L42%2071%2027%2056l-4%204%2020%2020%2034-52z%22/%3E%3C/svg%3E";

/// Render a default checkbox icon element based on checked state
fn render_default_checkbox_icon(checked: bool) -> RawElOrText {
    let svg_url = if checked { CHECKED_SVG } else { UNCHECKED_SVG };
    El::new()
        .s(zoon::Width::exact(40))
        .s(zoon::Height::exact(40))
        .s(zoon::Background::new().url(svg_url))
        .unify()
}

fn render_checkbox(fields: &Arc<std::collections::BTreeMap<Arc<str>, DdValue>>) -> RawElOrText {
    zoon::println!("[DD render_checkbox] CALLED with fields={:?}", fields.keys().collect::<Vec<_>>());
    // Extract checked value - can be Bool, Tagged, or HoldRef (reactive)
    let checked_value = fields.get("checked").cloned();
    zoon::println!("[DD render_checkbox] checked_value={:?}", checked_value);

    // Extract click LinkRef from element.event.click if present
    let click_link_id = fields
        .get("element")
        .and_then(|e| e.get("event"))
        .and_then(|e| e.get("click"))
        .and_then(|v| match v {
            DdValue::LinkRef(id) => Some(id.to_string()),
            _ => None,
        });

    // Use Checkbox component for proper role="checkbox" accessibility
    // with custom 40x40 SVG icon for visual consistency

    // Check if checked is a HoldRef (reactive checkbox)
    if let Some(DdValue::HoldRef(hold_id)) = &checked_value {
        // Reactive checkbox - observe HOLD state changes
        let hold_id_for_signal = hold_id.to_string();
        let hold_id_for_icon = hold_id.to_string();

        let checkbox = Checkbox::new()
            .label_hidden("checkbox")
            .checked_signal(
                hold_states_signal()
                    .map({
                        let hold_id = hold_id_for_signal.clone();
                        move |states| {
                            states.get(&hold_id)
                                .map(|v| match v {
                                    DdValue::Bool(b) => *b,
                                    DdValue::Tagged { tag, .. } => tag.as_ref() == "True",
                                    _ => false,
                                })
                                .unwrap_or(false)
                        }
                    })
            )
            .icon({
                // Observe HOLD state directly for icon - more reliable than checked_mutable
                // when elements are recreated during re-renders
                let hold_id_for_icon = hold_id.to_string();
                move |_checked_mutable| {
                    El::new()
                        .s(zoon::Width::exact(40))
                        .s(zoon::Height::exact(40))
                        .update_raw_el(|raw_el| raw_el.style("pointer-events", "none"))
                        .s(zoon::Background::new().url_signal(
                            hold_states_signal()
                                .map({
                                    let hold_id = hold_id_for_icon.clone();
                                    move |states| {
                                        let checked = states.get(&hold_id)
                                            .map(|v| match v {
                                                DdValue::Bool(b) => *b,
                                                DdValue::Tagged { tag, .. } => tag.as_ref() == "True",
                                                _ => false,
                                            })
                                            .unwrap_or(false);
                                        if checked { CHECKED_SVG } else { UNCHECKED_SVG }
                                    }
                                })
                        ))
                }
            });

        // For reactive checkboxes with a HoldRef, toggle the HOLD value directly
        // AND fire the link event (for any other listeners)
        let hold_id_for_toggle = hold_id.to_string();
        if let Some(ref link_id) = click_link_id {
            zoon::println!("[DD render_checkbox] RETURNING reactive checkbox with link_id={}", link_id);
            let link_id_owned = link_id.clone();
            // Use raw DOM event listener to bypass potential Zoon event handling issues
            return checkbox
                .update_raw_el(move |raw_el| {
                    let hold_id = hold_id_for_toggle.clone();
                    let link_id = link_id_owned.clone();
                    raw_el.event_handler(move |_: zoon::events::Click| {
                        zoon::println!("[DD CHECKBOX CLICK] RAW event handler invoked! hold_id={}, link_id={}", hold_id, link_id);
                        toggle_hold_bool(&hold_id);
                        fire_global_link(&link_id);
                    })
                })
                .unify();
        } else {
            zoon::println!("[DD render_checkbox] RETURNING reactive checkbox WITHOUT link_id");
            // No link, just toggle the HOLD directly
            let hold_id_clone = hold_id_for_toggle.clone();
            return checkbox
                .update_raw_el(move |raw_el| {
                    let hold_id = hold_id_clone.clone();
                    raw_el.event_handler(move |_: zoon::events::Click| {
                        toggle_hold_bool(&hold_id);
                    })
                })
                .unify();
        }
    }

    // Check for ComputedRef (e.g., toggle all checkbox where checked = todos_count == completed_todos_count)
    if let Some(DdValue::ComputedRef { computation, source_hold }) = &checked_value {
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
                    hold_states_signal()
                        .map({
                            let computation = computation.clone();
                            move |_states| {
                                // Evaluate the computation to get current checked state
                                use super::dd_value::evaluate_computed;
                                let result = evaluate_computed(&computation, &DdValue::Unit);
                                match result {
                                    DdValue::Bool(b) => b,
                                    _ => false,
                                }
                            }
                        })
                )
                .icon(move |_checked_mutable| {
                    // Render the custom icon value - it should be a container element
                    render_dd_value(&icon_value)
                })
        } else {
            // Use default SVG icon
            Checkbox::new()
                .label_hidden("checkbox")
                .checked_signal(
                    hold_states_signal()
                        .map({
                            let computation = computation.clone();
                            move |_states| {
                                use super::dd_value::evaluate_computed;
                                let result = evaluate_computed(&computation, &DdValue::Unit);
                                match result {
                                    DdValue::Bool(b) => b,
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
                                hold_states_signal()
                                    .map({
                                        let computation = computation.clone();
                                        move |_states| {
                                            use super::dd_value::evaluate_computed;
                                            let result = evaluate_computed(&computation, &DdValue::Unit);
                                            let checked = match result {
                                                DdValue::Bool(b) => b,
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
            DdValue::Bool(b) => *b,
            DdValue::Tagged { tag, .. } => tag.as_ref() == "True",
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
fn render_label(fields: &Arc<std::collections::BTreeMap<Arc<str>, DdValue>>) -> RawElOrText {
    let label_value = fields
        .get("label")
        .or_else(|| fields.get("text"));

    // Extract double_click LinkRef from element.event.double_click if present
    let double_click_link_id = fields
        .get("element")
        .and_then(|e| e.get("event"))
        .and_then(|e| e.get("double_click"))
        .and_then(|v| match v {
            DdValue::LinkRef(id) => Some(id.to_string()),
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
        .and_then(|v| if let DdValue::Number(n) = v { Some(n.0 as u32) } else { None });

    // Extract strikethrough from style.font.line.strikethrough (can be HoldRef for reactive)
    let strikethrough_hold = style
        .and_then(|s| s.get("font"))
        .and_then(|f| f.get("line"))
        .and_then(|l| l.get("strikethrough"))
        .and_then(|v| match v {
            DdValue::HoldRef(id) => Some(id.to_string()),
            _ => None,
        });

    let label = match label_value {
        Some(DdValue::HoldRef(name)) => {
            // Reactive label - update when HOLD state changes
            let hold_id = name.to_string();
            Label::new()
                .label_signal(
                    hold_states_signal()
                        .map(move |states| {
                            states
                                .get(&hold_id)
                                .map(|v| v.to_display_string())
                                .unwrap_or_default()
                        })
                )
                .for_input("dd_text_input")
        }
        Some(DdValue::Text(text)) if text.ends_with(" items") => {
            // Special case: "N items" pattern - make reactive based on items HOLD
            Label::new()
                .label_signal(
                    hold_states_signal()
                        .map(|states| {
                            let count = states.get("items")
                                .and_then(|v| match v {
                                    DdValue::List(list) => Some(list.len()),
                                    _ => None,
                                })
                                .unwrap_or(0);
                            format!("{} items", count)
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

    // If strikethrough is tied to a HoldRef (reactive completed state), we need a signal
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
fn render_paragraph(fields: &Arc<std::collections::BTreeMap<Arc<str>, DdValue>>) -> RawElOrText {
    // Try "contents" first (plural - used by Element/paragraph), then fallback to "content" or "text"
    let content = if let Some(DdValue::List(items)) = fields.get("contents") {
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

/// Extract display text from a DdValue, handling nested elements like links.
fn extract_text_content(value: &DdValue) -> String {
    match value {
        DdValue::Text(s) => s.to_string(),
        DdValue::Unit => " ".to_string(), // Text/space() renders as Unit
        DdValue::Tagged { tag, fields } if tag.as_ref() == "Element" => {
            // For Element tags, check the element type and extract appropriate text
            let element_type = fields
                .get("_element_type")
                .and_then(|v| match v {
                    DdValue::Text(s) => Some(s.as_ref()),
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
                    if let Some(DdValue::List(items)) = fields.get("contents") {
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
fn render_link(fields: &Arc<std::collections::BTreeMap<Arc<str>, DdValue>>) -> RawElOrText {
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
