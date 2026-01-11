//! DD Bridge - Renders DdValues to Zoon DOM elements.
//!
//! This is the DD equivalent of bridge.rs, but works with
//! simple DdValue types instead of actor-based Values.

use std::collections::BTreeMap;
use std::sync::Arc;

use zoon::*;

use super::dd_value::DdValue;
use super::dd_link::LinkId;
use super::dd_reactive_eval::{DdReactiveContext as GenericReactiveContext, HoldId, SumAccumulatorId};
use super::dd_interpreter::{DdReactiveResult, run_dd_reactive_with_context};

// ============================================================================
// Helper Functions
// ============================================================================

/// Convert an Oklch color DdValue to a CSS string.
/// Returns None if the value is not an Oklch color tag.
fn oklch_to_css(value: &DdValue) -> Option<String> {
    if let DdValue::Tagged { tag, fields } = value {
        if tag.as_ref() == "Oklch" {
            let lightness = fields.get("lightness")
                .and_then(|v| v.as_float())
                .unwrap_or(0.5);
            let chroma = fields.get("chroma")
                .and_then(|v| v.as_float())
                .unwrap_or(0.0);
            let hue = fields.get("hue")
                .and_then(|v| v.as_float())
                .unwrap_or(0.0);
            let alpha = fields.get("alpha")
                .and_then(|v| v.as_float());

            if let Some(a) = alpha {
                if a < 1.0 {
                    return Some(format!("oklch({}% {} {} / {})", lightness * 100.0, chroma, hue, a));
                }
            }
            return Some(format!("oklch({}% {} {})", lightness * 100.0, chroma, hue));
        }
    }
    None
}

/// Extract outline CSS from a style object.
/// Returns "none" for NoOutline tag, or a CSS outline string for outline Object.
fn extract_outline_css(style: Option<&DdValue>) -> Option<String> {
    let style = style?;
    let outline = style.get("outline")?;

    // Check for NoOutline tag
    if let DdValue::Tagged { tag, .. } = outline {
        if tag.as_ref() == "NoOutline" {
            return Some("none".to_string());
        }
    }

    // Check for outline Object with color
    if let Some(color) = outline.get("color") {
        if let Some(css_color) = oklch_to_css(color) {
            // Default outline: 1px solid color
            return Some(format!("1px solid {}", css_color));
        }
    }

    None
}

// ============================================================================
// Generic Reactive Bridge
// ============================================================================

/// Render a DdValue document using the generic reactive context.
///
/// This is the generic implementation that works with any Boon program.
pub fn render_dd_document_reactive(
    document: &DdValue,
    ctx: &GenericReactiveContext,
) -> RawElOrText {
    render_reactive_element(document, ctx, "")
}

/// Recursively render a DdValue with reactive context.
fn render_reactive_element(
    value: &DdValue,
    ctx: &GenericReactiveContext,
    path: &str,
) -> RawElOrText {
    // Log what we're rendering for debugging
    let value_type = match value {
        DdValue::Text(_) => "Text",
        DdValue::Number(_) => "Number",
        DdValue::Bool(_) => "Bool",
        DdValue::Unit => "Unit",
        DdValue::HoldRef(name) => {
            zoon::println!("[render_element] HoldRef({})", name);
            "HoldRef"
        }
        DdValue::Tagged { tag, .. } => {
            zoon::println!("[render_element] Tagged({})", tag);
            tag.as_ref()
        }
        DdValue::Object(_) => "Object",
        DdValue::List(items) => {
            zoon::println!("[render_element] List(len={})", items.len());
            "List"
        }
    };
    let _ = value_type; // suppress unused warning

    match value {
        DdValue::Text(s) => zoon::Text::new(s.as_ref()).unify(),
        DdValue::Number(n) => zoon::Text::new(n.to_string()).unify(),
        DdValue::Bool(b) => zoon::Text::new(b.to_string()).unify(),
        DdValue::Unit => zoon::Text::new("").unify(),

        // HoldRef - look up current value from reactive context
        DdValue::HoldRef(hold_name) => {
            let hold_id = HoldId::new(hold_name.as_ref());
            if let Some(signal) = ctx.get_hold(&hold_id) {
                let current = signal.get();
                // Recursively render the current value
                render_reactive_element(&current, ctx, path)
            } else {
                zoon::Text::new(format!("[hold:{}]", hold_name)).unify()
            }
        }

        DdValue::Tagged { tag, fields } => {
            match tag.as_ref() {
                "Element" => render_reactive_typed_element(fields, ctx, path),
                "NoElement" => El::new().unify(),
                // __HoldWithSkip__ marker: resolve to HOLD value with skip checking
                "__HoldWithSkip__" => {
                    if let Some(DdValue::Text(hold_name)) = fields.get("hold_id") {
                        let hold_id = HoldId::new(hold_name.as_ref());
                        // Get value with skip checking - returns Unit if still skipping
                        let value = ctx.get_hold_value_with_skip(&hold_id);
                        render_reactive_element(&value, ctx, path)
                    } else {
                        zoon::Text::new("").unify()
                    }
                }
                // __ReactiveSum__ marker: resolve to sum accumulator value
                "__ReactiveSum__" => {
                    if let Some(DdValue::Text(acc_id_str)) = fields.get("accumulator_id") {
                        let acc_id = SumAccumulatorId::new(acc_id_str.as_ref());
                        if let Some(signal) = ctx.get_sum_accumulator(&acc_id) {
                            let value = signal.get();
                            // Unit means no value yet (timer hasn't fired) - render empty
                            if value == DdValue::Unit {
                                zoon::Text::new("").unify()
                            } else {
                                render_reactive_element(&value, ctx, path)
                            }
                        } else {
                            zoon::Text::new("").unify()
                        }
                    } else {
                        zoon::Text::new("").unify()
                    }
                }
                _ => zoon::Text::new(tag.as_ref()).unify(),
            }
        }

        DdValue::Object(_obj) => {
            // Render object as debug text
            zoon::Text::new("[object]").unify()
        }

        DdValue::List(items) => {
            if items.is_empty() {
                El::new().unify()
            } else {
                let children: Vec<RawElOrText> = items
                    .iter()
                    .enumerate()
                    .map(|(i, item)| {
                        let item_path = format!("{}/items[{}]", path, i);
                        render_reactive_element(item, ctx, &item_path)
                    })
                    .collect();
                Column::new().items(children).unify()
            }
        }
    }
}

/// Render a typed Element with reactive context.
fn render_reactive_typed_element(
    fields: &Arc<BTreeMap<Arc<str>, DdValue>>,
    ctx: &GenericReactiveContext,
    path: &str,
) -> RawElOrText {
    let element_type = fields
        .get("_element_type")
        .and_then(|v| v.as_text())
        .unwrap_or("container");

    zoon::println!("[render_typed_element] type='{}' path='{}'", element_type, path);

    match element_type {
        "container" => render_reactive_container(fields, ctx, path),
        "stripe" => render_reactive_stripe(fields, ctx, path),
        "stack" => render_reactive_stack(fields, ctx, path),
        "button" => render_reactive_button(fields, ctx, path),
        "text_input" => render_reactive_text_input(fields, ctx, path),
        "checkbox" => render_reactive_checkbox(fields, ctx, path),
        "label" => render_reactive_label(fields, ctx),
        "paragraph" => render_reactive_paragraph(fields, ctx, path),
        "link" => render_reactive_link(fields),
        _ => render_reactive_container(fields, ctx, path),
    }
}

/// Render a container element.
fn render_reactive_container(
    fields: &Arc<BTreeMap<Arc<str>, DdValue>>,
    ctx: &GenericReactiveContext,
    path: &str,
) -> RawElOrText {
    let style = fields.get("style");
    let size = style.and_then(|s| s.get("size")).and_then(|v| v.as_float());
    let width = style.and_then(|s| s.get("width")).and_then(|v| v.as_float());
    let height = style.and_then(|s| s.get("height")).and_then(|v| v.as_float());
    let bg_url = style
        .and_then(|s| s.get("background"))
        .and_then(|b| b.get("url"))
        .and_then(|u| match u {
            DdValue::Text(t) => Some(t.to_string()),
            _ => None,
        });

    // Extract font styling
    let font_color_css = style
        .and_then(|s| s.get("font"))
        .and_then(|f| f.get("color"))
        .and_then(|c| oklch_to_css(c));
    let font_size = style
        .and_then(|s| s.get("font"))
        .and_then(|f| f.get("size"))
        .and_then(|v| v.as_float());

    // Extract padding
    let padding_column = style
        .and_then(|s| s.get("padding"))
        .and_then(|p| p.get("column"))
        .and_then(|v| v.as_float());
    let padding_row = style
        .and_then(|s| s.get("padding"))
        .and_then(|p| p.get("row"))
        .and_then(|v| v.as_float());

    // Extract transform (rotation)
    let rotate = style
        .and_then(|s| s.get("transform"))
        .and_then(|t| t.get("rotate"))
        .and_then(|v| v.as_float());

    let mut el = El::new();
    if let Some(s) = size {
        el = el.s(Width::exact(s as u32)).s(Height::exact(s as u32));
    }
    if let Some(w) = width {
        el = el.s(Width::exact(w as u32));
    }
    if let Some(h) = height {
        el = el.s(Height::exact(h as u32));
    }
    if let Some(url) = bg_url {
        el = el.s(Background::new().url(&url));
    }

    // Apply font color via raw style
    if let Some(color) = font_color_css {
        el = el.update_raw_el(move |raw| raw.style("color", &color));
    }
    // Apply font size
    if let Some(fs) = font_size {
        let fs_css = format!("{}px", fs);
        el = el.update_raw_el(move |raw| raw.style("font-size", &fs_css));
    }
    // Apply padding
    if let (Some(pc), Some(pr)) = (padding_column, padding_row) {
        let padding_css = format!("{}px {}px", pc, pr);
        el = el.update_raw_el(move |raw| raw.style("padding", &padding_css));
    }
    // Apply rotation transform
    if let Some(deg) = rotate {
        let transform_css = format!("rotate({}deg)", deg);
        el = el.update_raw_el(move |raw| raw.style("transform", &transform_css));
    }

    if let Some(child_value) = fields.get("child") {
        let child_path = format!("{}/child", path);
        let child_el = render_reactive_element(child_value, ctx, &child_path);
        el.child(child_el).unify()
    } else {
        el.unify()
    }
}

/// Render a stripe (column/row) element.
fn render_reactive_stripe(
    fields: &Arc<BTreeMap<Arc<str>, DdValue>>,
    ctx: &GenericReactiveContext,
    path: &str,
) -> RawElOrText {
    let direction = fields
        .get("direction")
        .and_then(|v| match v {
            DdValue::Tagged { tag, .. } => Some(tag.as_ref()),
            _ => None,
        })
        .unwrap_or("Column");

    let items = fields.get("items").and_then(|v| v.as_list()).unwrap_or(&[]);

    let gap = fields
        .get("gap")
        .and_then(|v| v.as_int())
        .unwrap_or(0) as u32;

    let children: Vec<RawElOrText> = items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let item_path = format!("{}/items[{}]", path, i);
            render_reactive_element(item, ctx, &item_path)
        })
        .collect();

    match direction {
        "Column" => Column::new().s(Gap::both(gap)).items(children).unify(),
        "Row" => Row::new().s(Gap::both(gap)).items(children).unify(),
        _ => Column::new().s(Gap::both(gap)).items(children).unify(),
    }
}

/// Render a stack element.
fn render_reactive_stack(
    fields: &Arc<BTreeMap<Arc<str>, DdValue>>,
    ctx: &GenericReactiveContext,
    path: &str,
) -> RawElOrText {
    let layers = fields.get("layers").and_then(|v| v.as_list()).unwrap_or(&[]);
    let children: Vec<RawElOrText> = layers
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let item_path = format!("{}/layers[{}]", path, i);
            render_reactive_element(item, ctx, &item_path)
        })
        .collect();

    Stack::new().layers(children).unify()
}

/// Render a button element with reactive LINK handling.
fn render_reactive_button(
    fields: &Arc<BTreeMap<Arc<str>, DdValue>>,
    ctx: &GenericReactiveContext,
    _path: &str,
) -> RawElOrText {
    let label = fields
        .get("label")
        .map(|v| v.to_display_string())
        .unwrap_or_default();

    // Extract outline CSS from style
    let outline_css = extract_outline_css(fields.get("style"));

    // Check for LINK in element.event.press
    let link_name = extract_link_name(fields);

    // Build press handler closure
    let ctx_clone = ctx.clone();
    let link_name_for_handler = link_name.clone();

    let press_handler = move || {
        zoon::println!("[press_handler] Button pressed! link_name_for_handler={:?}", link_name_for_handler);
        if let Some(ref link_id_str) = link_name_for_handler {
            let link_id = LinkId::new(link_id_str);

            // Fire the LINK event (base LINK)
            ctx_clone.fire_link(&link_id);

            // Also fire with .event.press suffix for List/clear(on: X.event.press) patterns
            let press_link_id = LinkId::new(format!("{}.event.press", link_id_str));
            ctx_clone.fire_link_with_value(&press_link_id, DdValue::Unit);
            zoon::println!("[press_handler] Fired .event.press: {:?}", press_link_id);

            // Handle LINK → THEN → HOLD connections
            // This evaluates the THEN body and updates the associated HOLD state
            ctx_clone.handle_link_fire(&link_id);
            ctx_clone.handle_link_fire(&press_link_id);

            // Trigger re-render
            ctx_clone.trigger_render();

            // Clear the .event.press LINK after render completes (async)
            // Use spawn_local to defer to next microtask, after Zoon's render
            let ctx_for_clear = ctx_clone.clone();
            wasm_bindgen_futures::spawn_local(async move {
                ctx_for_clear.clear_link_value(&press_link_id);
                zoon::println!("[press_handler] Cleared .event.press after render (deferred)");
            });
        } else {
            // Trigger re-render (no link to clear)
            ctx_clone.trigger_render();
        }
    };

    // Build button - wrap label in El like checkbox does
    zoon::println!("[render_reactive_button] Creating button label='{}' link_name={:?} outline_css={:?}",
        label, link_name, outline_css);

    // Apply outline CSS if present, otherwise use default styling
    // Use zoon::Text directly as label (no El wrapper) so clicks hit the button container
    if let Some(outline) = outline_css {
        Button::new()
            .label(zoon::Text::new(&label))
            .on_press(press_handler)
            .update_raw_el(|raw_el| raw_el.attr("role", "button"))
            .update_raw_el(move |raw_el| raw_el.style("outline", &outline))
            .unify()
    } else {
        Button::new()
            .label(zoon::Text::new(&label))
            .on_press(press_handler)
            .update_raw_el(|raw_el| raw_el.attr("role", "button"))
            .unify()
    }
}

/// Extract LINK variable name from element fields.
fn extract_link_name(fields: &Arc<BTreeMap<Arc<str>, DdValue>>) -> Option<String> {
    zoon::println!("[extract_link_name] fields keys: {:?}", fields.keys().collect::<Vec<_>>());
    // First, check for injected __link_id__ (set by evaluator for elements with LINK)
    if let Some(DdValue::Text(link_id)) = fields.get("__link_id__") {
        zoon::println!("[extract_link_name] Found __link_id__: {}", link_id);
        return Some(link_id.to_string());
    }
    zoon::println!("[extract_link_name] No __link_id__ found");

    // Look for element.event.press containing LINK marker
    let element = fields.get("element")?;
    let event = element.get("event")?;

    // Check press event
    if let Some(press) = event.get("press") {
        // LINK is represented as Unit or Tagged{LINK}
        if matches!(press, DdValue::Unit) {
            // Fallback to default name if no __link_id__ was injected
            return Some("button_press".to_string());
        }
        if let DdValue::Tagged { tag, fields } = press {
            if tag.as_ref() == "LINK" {
                // Extract name from LINK fields if present
                if let Some(name) = fields.get("name") {
                    if let DdValue::Text(s) = name {
                        return Some(s.to_string());
                    }
                }
                return Some("button_press".to_string());
            }
        }
    }

    // Check click event (for checkboxes)
    if let Some(click) = event.get("click") {
        // LINK is represented as Unit or Tagged{LINK}
        if matches!(click, DdValue::Unit) {
            // Fallback to default name if no __link_id__ was injected
            return Some("checkbox_click".to_string());
        }
        if let DdValue::Tagged { tag, fields } = click {
            if tag.as_ref() == "LINK" {
                // Extract name from LINK fields if present
                if let Some(name) = fields.get("name") {
                    if let DdValue::Text(s) = name {
                        return Some(s.to_string());
                    }
                }
                return Some("checkbox_click".to_string());
            }
        }
    }

    // Check for link_id field directly
    if let Some(link_id) = fields.get("link_id") {
        if let DdValue::Text(s) = link_id {
            return Some(s.to_string());
        }
    }

    None
}

/// Render a text input element.
fn render_reactive_text_input(
    fields: &Arc<BTreeMap<Arc<str>, DdValue>>,
    ctx: &GenericReactiveContext,
    _path: &str,
) -> RawElOrText {
    zoon::println!("[render_reactive_text_input] fields keys: {:?}", fields.keys().collect::<Vec<_>>());
    if let Some(link_id) = fields.get("__link_id__") {
        zoon::println!("[render_reactive_text_input] __link_id__ = {:?}", link_id);
    } else {
        zoon::println!("[render_reactive_text_input] NO __link_id__ found!");
    }

    let placeholder_text = fields
        .get("placeholder")
        .and_then(|v| v.get("text"))
        .map(|v| v.to_display_string())
        .unwrap_or_default();

    let text_value = fields
        .get("text")
        .map(|v| v.to_display_string())
        .unwrap_or_default();

    let focus_value = fields.get("focus");
    zoon::println!("[render_reactive_text_input] focus field value: {:?}", focus_value);
    let should_focus = focus_value
        .map(|v| {
            let truthy = v.is_truthy();
            zoon::println!("[render_reactive_text_input] focus is_truthy={}", truthy);
            truthy
        })
        .unwrap_or(false);
    zoon::println!("[render_reactive_text_input] should_focus={}", should_focus);

    // Extract key_down LINK ID if present
    let key_down_link_id = fields.get("__link_id__")
        .and_then(|v| match v {
            DdValue::Text(s) => Some(s.to_string()),
            _ => None,
        });

    let text_mutable = Mutable::new(text_value);
    let text_for_change = text_mutable.clone();

    // Fire change event and .text property update when text changes
    let ctx_for_change = ctx.clone();
    let change_link_id = key_down_link_id.clone();
    let on_change_handler = move |new_text: String| {
        text_for_change.set(new_text.clone());

        if let Some(ref link_id_str) = change_link_id {
            // Fire change.text event
            let change_text_link_id = format!("{}.event.change.text", link_id_str);
            ctx_for_change.fire_link_with_value(&LinkId::new(&change_text_link_id), DdValue::text(new_text.as_str()));

            // Also fire .text property so elements.item_input.text works
            let text_link_id = format!("{}.text", link_id_str);
            ctx_for_change.fire_link_with_value(&LinkId::new(&text_link_id), DdValue::text(new_text.as_str()));

            zoon::println!("[text_input on_change] text='{}' link_id={}", new_text, link_id_str);
        }
    };

    // Helper macro to add key_down handler
    macro_rules! add_key_handler {
        ($input:expr, $ctx:expr, $link_id:expr) => {{
            let ctx_clone = $ctx.clone();
            let link_id_clone = $link_id.clone();
            $input.update_raw_el(move |raw_el| {
                let ctx_inner = ctx_clone.clone();
                let link_id_inner = link_id_clone.clone();
                raw_el.event_handler(move |event: zoon::events::KeyDown| {
                    let key = event.key();
                    zoon::println!("[text_input key_down] key='{}' link_id={:?}", key, link_id_inner);
                    if let Some(ref link_id_str) = link_id_inner {
                        let key_link_id = format!("{}.event.key_down.key", link_id_str);
                        let link_id = LinkId::new(&key_link_id);
                        // Fire as a Tagged value so WHEN { Enter => ... } patterns match
                        let key_value = DdValue::tagged(key.as_str(), std::iter::empty::<(&str, DdValue)>());
                        ctx_inner.fire_link_with_value(&link_id, key_value.clone());
                        ctx_inner.handle_link_fire(&link_id);

                        // Only trigger render for special keys (Enter, Tab, Escape, etc.)
                        // Regular character keys should NOT trigger render because:
                        // 1. The character hasn't been inserted yet when keyDown fires
                        // 2. Triggering render would recreate the input with stale text
                        // 3. The browser would then insert the char into a replaced element
                        let is_special_key = matches!(key.as_str(),
                            "Enter" | "Tab" | "Escape" | "Backspace" | "Delete" |
                            "ArrowUp" | "ArrowDown" | "ArrowLeft" | "ArrowRight" |
                            "Home" | "End" | "PageUp" | "PageDown"
                        );

                        if is_special_key {
                            ctx_inner.trigger_render();

                            // Clear the key event after render completes (async)
                            // Use spawn_local to defer to next microtask, after Zoon's render
                            let ctx_for_clear = ctx_inner.clone();
                            wasm_bindgen_futures::spawn_local(async move {
                                ctx_for_clear.clear_link_value(&link_id);
                                zoon::println!("[text_input key_down] Cleared key event after render (deferred)");
                            });
                        }
                    }
                })
            })
        }};
    }

    // Build text input - placeholder requires different type state
    // For focus, use after_insert to focus directly via web_sys after element is in DOM
    // .focus(true) sets autofocus which only works on page load
    // .focus_signal() doesn't work with re-renders that replace elements
    if placeholder_text.is_empty() {
        let input = TextInput::new()
            .label_hidden("text input")
            .text_signal(text_mutable.signal_cloned())
            .on_change(on_change_handler);
        let input = add_key_handler!(input, ctx, key_down_link_id);
        if should_focus {
            input.update_raw_el(|raw_el| {
                raw_el.after_insert(move |element| {
                    // Focus the input element directly using web_sys
                    // Note: TextInput wraps the <input> in a <div>, so we need to find the child input
                    zoon::println!("[render_reactive_text_input] after_insert element tag: {:?}", element.tag_name());
                    if let Some(input_el) = element.dyn_ref::<web_sys::HtmlInputElement>() {
                        let _ = input_el.focus();
                        zoon::println!("[render_reactive_text_input] Focused input via after_insert (direct)");
                    } else if let Some(div_el) = element.dyn_ref::<web_sys::HtmlElement>() {
                        // Try to find the input child
                        if let Some(input_el) = div_el.query_selector("input").ok().flatten() {
                            if let Some(input_el) = input_el.dyn_ref::<web_sys::HtmlInputElement>() {
                                let _ = input_el.focus();
                                zoon::println!("[render_reactive_text_input] Focused input via after_insert (child query)");
                            }
                        }
                    }
                })
            }).unify()
        } else {
            input.unify()
        }
    } else {
        let input = TextInput::new()
            .label_hidden("text input")
            .text_signal(text_mutable.signal_cloned())
            .placeholder(Placeholder::new(&placeholder_text))
            .on_change(on_change_handler);
        let input = add_key_handler!(input, ctx, key_down_link_id);
        if should_focus {
            input.update_raw_el(|raw_el| {
                raw_el.after_insert(move |element| {
                    // Focus the input element directly using web_sys
                    // Note: TextInput wraps the <input> in a <div>, so we need to find the child input
                    zoon::println!("[render_reactive_text_input] after_insert element tag: {:?}", element.tag_name());
                    if let Some(input_el) = element.dyn_ref::<web_sys::HtmlInputElement>() {
                        let _ = input_el.focus();
                        zoon::println!("[render_reactive_text_input] Focused input via after_insert (direct)");
                    } else if let Some(div_el) = element.dyn_ref::<web_sys::HtmlElement>() {
                        // Try to find the input child
                        if let Some(input_el) = div_el.query_selector("input").ok().flatten() {
                            if let Some(input_el) = input_el.dyn_ref::<web_sys::HtmlInputElement>() {
                                let _ = input_el.focus();
                                zoon::println!("[render_reactive_text_input] Focused input via after_insert (child query)");
                            }
                        }
                    }
                })
            }).unify()
        } else {
            input.unify()
        }
    }
}

/// Render a checkbox element with reactive LINK handling.
fn render_reactive_checkbox(
    fields: &Arc<BTreeMap<Arc<str>, DdValue>>,
    ctx: &GenericReactiveContext,
    path: &str,
) -> RawElOrText {
    // Resolve HoldRef for checked status
    let checked_value = fields.get("checked");
    let checked = match checked_value {
        Some(DdValue::HoldRef(hold_name)) => {
            let hold_id = HoldId::new(hold_name.as_ref());
            ctx.get_hold(&hold_id)
                .map(|s| s.get().is_truthy())
                .unwrap_or(false)
        }
        Some(v) => v.is_truthy(),
        None => false,
    };

    // Extract LINK name for toggle event (same as button)
    let link_name = extract_link_name(fields);

    // Build click handler closure
    let ctx_clone = ctx.clone();
    let link_name_for_handler = link_name.clone();

    let click_handler = move || {
        zoon::println!("[checkbox click_handler] Clicked! link_name={:?}", link_name_for_handler);
        if let Some(ref link_id_str) = link_name_for_handler {
            let link_id = LinkId::new(link_id_str);

            // Fire the LINK event
            ctx_clone.fire_link(&link_id);
            zoon::println!("[checkbox click_handler] Fired link {:?}", link_id);

            // Handle LINK → THEN → HOLD connections
            ctx_clone.handle_link_fire(&link_id);
            zoon::println!("[checkbox click_handler] Handled link fire");
        }
        // Trigger re-render
        zoon::println!("[checkbox click_handler] About to trigger_render");
        ctx_clone.trigger_render();
        zoon::println!("[checkbox click_handler] trigger_render called");
    };

    // Render the icon - need to handle dynamic checked state
    // The icon may have a pre-computed background URL that doesn't reflect current checked state
    let icon_element = if let Some(icon) = fields.get("icon") {
        // Check if icon is a container with a background URL
        if let DdValue::Tagged { tag, fields: icon_fields } = icon {
            if tag.as_ref() == "Element" {
                // Extract the background URL and potentially override based on checked state
                let style = icon_fields.get("style");
                let bg_url = style
                    .and_then(|s| s.get("background"))
                    .and_then(|b| b.get("url"))
                    .and_then(|u| u.as_text());

                // Check if this looks like a TodoMVC checkbox icon (data:image/svg+xml)
                if let Some(url) = bg_url {
                    if url.contains("data:image/svg+xml") {
                        // TodoMVC uses different SVGs for checked/unchecked
                        // Override based on current checked state
                        let size = style
                            .and_then(|s| s.get("size"))
                            .and_then(|v| v.as_float())
                            .unwrap_or(40.0);

                        let actual_url = if checked {
                            // Green checkmark
                            "data:image/svg+xml;utf8,%3Csvg%20xmlns%3D%22http%3A//www.w3.org/2000/svg%22%20width%3D%2240%22%20height%3D%2240%22%20viewBox%3D%22-10%20-18%20100%20135%22%3E%3Ccircle%20cx%3D%2250%22%20cy%3D%2250%22%20r%3D%2250%22%20fill%3D%22none%22%20stroke%3D%22%23bddad5%22%20stroke-width%3D%223%22/%3E%3Cpath%20fill%3D%22%235dc2af%22%20d%3D%22M72%2025L42%2071%2027%2056l-4%204%2020%2020%2034-52z%22/%3E%3C/svg%3E"
                        } else {
                            // Gray circle
                            "data:image/svg+xml;utf8,%3Csvg%20xmlns%3D%22http%3A//www.w3.org/2000/svg%22%20width%3D%2240%22%20height%3D%2240%22%20viewBox%3D%22-10%20-18%20100%20135%22%3E%3Ccircle%20cx%3D%2250%22%20cy%3D%2250%22%20r%3D%2250%22%20fill%3D%22none%22%20stroke%3D%22%23ededed%22%20stroke-width%3D%223%22/%3E%3C/svg%3E"
                        };

                        return Button::new()
                            .label(
                                El::new()
                                    .s(Width::exact(size as u32))
                                    .s(Height::exact(size as u32))
                                    .s(Background::new().url(actual_url))
                            )
                            .on_press(click_handler)
                            .update_raw_el(|raw| raw.attr("role", "checkbox"))
                            .unify();
                    }
                }
            }
        }
        // Default: render the icon as-is
        render_reactive_element(icon, ctx, &format!("{}.icon", path))
    } else {
        // Fallback: simple circle
        El::new()
            .s(Width::exact(20))
            .s(Height::exact(20))
            .s(RoundedCorners::all(10))
            .s(Borders::all(Border::new().width(2).color(hsluv!(0, 0, 80))))
            .unify()
    };

    // Use Button for reliable click handling with checkbox role
    Button::new()
        .label(icon_element)
        .on_press(click_handler)
        .update_raw_el(|raw| raw.attr("role", "checkbox"))
        .unify()
}

/// Render a label element.
fn render_reactive_label(
    fields: &Arc<BTreeMap<Arc<str>, DdValue>>,
    ctx: &GenericReactiveContext,
) -> RawElOrText {
    // Get label value, resolving HoldRef if present
    let label_value = fields.get("label");
    let text = match label_value {
        Some(DdValue::HoldRef(hold_name)) => {
            // Resolve HoldRef to its current value
            let hold_id = HoldId::new(hold_name.as_ref());
            ctx.get_hold(&hold_id)
                .map(|s| s.get().to_display_string())
                .unwrap_or_else(|| format!("[hold:{}]", hold_name))
        }
        Some(v) => v.to_display_string(),
        None => String::new(),
    };

    // Check if this label references a HOLD variable
    // Look for patterns like "{count}" or direct hold references
    let hold_values = ctx.get_hold_values();

    // Try to find a HOLD value that matches
    for (hold_name, _signal) in hold_values {
        if text.contains(&format!("{{{}}}", hold_name)) {
            // This label has a reactive reference
            // For now, just substitute the current value
            let current = ctx.get_hold(&HoldId::new(&hold_name))
                .map(|s| s.get().to_display_string())
                .unwrap_or_default();
            let rendered = text.replace(&format!("{{{}}}", hold_name), &current);
            return Label::new().label(zoon::Text::new(&rendered)).unify();
        }
    }

    Label::new().label(zoon::Text::new(&text)).unify()
}

/// Render a paragraph element.
fn render_reactive_paragraph(
    fields: &Arc<BTreeMap<Arc<str>, DdValue>>,
    ctx: &GenericReactiveContext,
    path: &str,
) -> RawElOrText {
    // Handle both "content" (single string) and "contents" (list of elements)
    if let Some(contents) = fields.get("contents").and_then(|v| v.as_list()) {
        // Render each item in the contents list as a child element
        let children: Vec<RawElOrText> = contents
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let item_path = format!("{}/contents[{}]", path, i);
                render_reactive_element(item, ctx, &item_path)
            })
            .collect();

        Paragraph::new().contents(children).unify()
    } else {
        // Fallback to single content string
        let text = fields
            .get("content")
            .map(|v| v.to_display_string())
            .unwrap_or_default();

        Paragraph::new().content(text).unify()
    }
}

/// Render a link element.
fn render_reactive_link(fields: &Arc<BTreeMap<Arc<str>, DdValue>>) -> RawElOrText {
    let label = fields
        .get("label")
        .map(|v| v.to_display_string())
        .unwrap_or_default();

    let url = fields
        .get("url")
        .and_then(|v| v.as_text())
        .unwrap_or("#");

    Link::new()
        .label(label)
        .to(url)
        .unify()
}

/// Render a document with signal-based reactivity.
///
/// This version re-renders when the reactive context's render trigger changes.
pub fn render_dd_document_reactive_signal(
    document: DdValue,
    ctx: GenericReactiveContext,
) -> impl Element {
    zoon::println!("[render_dd_document_reactive_signal] CALLED - this is for timer/accumulator examples");
    let ctx_for_render = ctx.clone();
    let doc_for_render = document;

    // Get the render signal first (before moving ctx)
    zoon::println!("[render_dd_document_reactive_signal] Getting render_signal from ctx");
    let render_signal = ctx.render_signal();

    // Start any registered timers
    start_timers(&ctx);

    El::new()
        .s(Width::fill())
        .s(Height::fill())
        .child_signal(
            render_signal.map(move |_trigger| {
                // Re-render the document with current HOLD values
                let rendered = render_dd_document_reactive(&doc_for_render, &ctx_for_render);

                // Wrap in El to satisfy trait bounds
                El::new().child(rendered).unify()
            })
        )
}

/// Render a DdReactiveResult with full re-evaluation on state changes.
///
/// This function re-evaluates the entire Boon program on each render trigger,
/// using the existing reactive context. When HOLDs are re-registered during
/// evaluation, `register_hold` returns the existing signal with its current
/// value. This ensures derived values like `active_todos_count` are recomputed.
pub fn render_dd_result_reactive_signal(
    result: DdReactiveResult,
) -> impl Element {
    zoon::println!("[render_dd_result_reactive_signal] CALLED - this is for HOLD-only examples like todo_mvc");
    let ctx = result.context.clone();
    let source_code = result.source_code.clone();
    let filename = result.filename.clone();
    let ctx_for_render = ctx.clone();
    let source_for_render = source_code.clone();
    let filename_for_render = filename.clone();

    // Get the render signal first (before moving ctx)
    zoon::println!("[render_dd_result_reactive_signal] Getting render_signal from ctx");
    let render_signal = ctx.render_signal();

    // Start any registered timers
    start_timers(&ctx);

    El::new()
        .s(Width::fill())
        .s(Height::fill())
        .child_signal(
            render_signal.map(move |trigger| {
                zoon::println!("[render_dd_result_reactive_signal] Trigger {}, re-evaluating...", trigger);

                // Re-evaluate the program with the existing reactive context.
                // This shares the context so HOLDs return their current values.
                let new_document = run_dd_reactive_with_context(
                    &filename_for_render,
                    &source_for_render,
                    ctx_for_render.clone(),
                );

                // Render the new document
                if let Some(doc) = new_document {
                    let rendered = render_dd_document_reactive(&doc, &ctx_for_render);
                    El::new().child(rendered).unify()
                } else {
                    // Parsing/evaluation error - show error placeholder
                    El::new().child(zoon::Text::new("Error re-evaluating document")).unify()
                }
            })
        )
}

/// Start all registered timers in the reactive context.
/// Each timer spawns a background task that fires periodically.
fn start_timers(ctx: &GenericReactiveContext) {
    // Prevent starting timers if they're already running
    // This can happen if render_dd_document_reactive_signal is called multiple times
    if ctx.has_timer_handles() {
        zoon::println!("[start_timers] Already has timer handles, skipping");
        return;
    }

    let timers = ctx.get_timers();
    zoon::println!("[start_timers] Starting {} timers, session={}, run_generation={}",
        timers.len(), ctx.session_id(), ctx.run_generation());

    for timer_info in timers {
        let ctx_for_timer = ctx.clone();
        let timer_id = timer_info.id.clone();
        let interval_ms = timer_info.interval_ms;
        zoon::println!("[start_timers] Starting timer {:?} with interval {}ms", timer_id, interval_ms);

        // Spawn a background task for this timer
        let sleep_ms = interval_ms.round().max(0.0).min(u32::MAX as f64) as u32;
        zoon::println!("[timer_loop] Starting loop, sleep_ms={}", sleep_ms);
        let handle = Task::start_droppable(async move {
            loop {
                zoon::println!("[timer_loop] About to sleep {}ms", sleep_ms);
                // Wait for the interval
                Timer::sleep(sleep_ms).await;
                zoon::println!("[timer_loop] Woke up, ctx.session={} gen={}, current.session={} gen={}",
                    ctx_for_timer.session_id(), ctx_for_timer.run_generation(),
                    super::dd_reactive_eval::session_id(), super::dd_reactive_eval::current_run_generation());

                // Check if this context is still the current run - if not, stop firing
                // This prevents old timers from continuing after a new run starts
                if !ctx_for_timer.is_current_run() {
                    zoon::println!("[timer_loop] Not current run, stopping");
                    break;
                }

                // Fire the timer and trigger render if state was updated
                if ctx_for_timer.handle_timer_fire(&timer_id) {
                    ctx_for_timer.trigger_render();
                }
            }
        });

        // Store the handle to keep the task alive
        ctx.add_timer_handle(handle);
    }
}
