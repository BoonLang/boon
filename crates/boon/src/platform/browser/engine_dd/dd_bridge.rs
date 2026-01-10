//! DD Bridge - Renders DdValues to Zoon DOM elements.
//!
//! This is the DD equivalent of bridge.rs, but works with
//! simple DdValue types instead of actor-based Values.

use std::collections::BTreeMap;
use std::sync::Arc;

use zoon::*;

use super::dd_value::DdValue;
use super::dd_link::LinkId;
use super::dd_reactive_eval::{DdReactiveContext as GenericReactiveContext, HoldId};

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

    match element_type {
        "container" => render_reactive_container(fields, ctx, path),
        "stripe" => render_reactive_stripe(fields, ctx, path),
        "stack" => render_reactive_stack(fields, ctx, path),
        "button" => render_reactive_button(fields, ctx, path),
        "text_input" => render_reactive_text_input(fields, ctx, path),
        "checkbox" => render_reactive_checkbox(fields, ctx, path),
        "label" => render_reactive_label(fields, ctx),
        "paragraph" => render_reactive_paragraph(fields),
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
    let label_for_handler = label.clone();
    let link_name_for_handler = link_name.clone();

    let press_handler = move || {
        if let Some(ref link_id_str) = link_name_for_handler {
            let link_id = LinkId::new(link_id_str);
            zoon::println!("[DD_REACTIVE] Button pressed: link={}", link_id_str);

            // Fire the LINK
            ctx_clone.fire_link(&link_id);

            // Check for special button types that update HOLD state
            let is_increment = label_for_handler.to_lowercase().contains("increment")
                || label_for_handler == "+"
                || link_id_str.contains("increment");
            let is_decrement = label_for_handler.to_lowercase().contains("decrement")
                || label_for_handler == "-"
                || link_id_str.contains("decrement");

            // Find the associated HOLD by looking for a pattern like "{holdname}_increment"
            let hold_name = if link_id_str.contains("_increment") {
                link_id_str.replace("_increment", "")
            } else if link_id_str.contains("_decrement") {
                link_id_str.replace("_decrement", "")
            } else {
                // Default to "counter" for simple cases
                "counter".to_string()
            };

            // Update the associated HOLD state
            let hold_id = HoldId::new(&hold_name);
            if let Some(signal) = ctx_clone.get_hold(&hold_id) {
                let current = signal.get();
                let current_for_log = current.clone();
                let new_value = match &current {
                    DdValue::Number(n) => {
                        if is_increment {
                            DdValue::float(n.0 + 1.0)
                        } else if is_decrement {
                            DdValue::float(n.0 - 1.0)
                        } else {
                            // Default: increment
                            DdValue::float(n.0 + 1.0)
                        }
                    }
                    _ => current,
                };
                zoon::println!("[DD_REACTIVE] Updating HOLD {}: {:?} -> {:?}",
                    hold_name, current_for_log, new_value);
                ctx_clone.update_hold(&hold_id, new_value);
            }
        }
        // Trigger re-render
        ctx_clone.trigger_render();
    };

    // Build button with on_press already wired
    // Typestate prevents reassignment after on_press(), so build in one chain
    if let Some(outline) = outline_css {
        Button::new()
            .label(zoon::Text::new(&label))
            .update_raw_el(|raw_el| raw_el.attr("role", "button"))
            .on_press(press_handler)
            .update_raw_el(move |raw_el| raw_el.style("outline", &outline))
            .unify()
    } else {
        Button::new()
            .label(zoon::Text::new(&label))
            .update_raw_el(|raw_el| raw_el.attr("role", "button"))
            .on_press(press_handler)
            .unify()
    }
}

/// Extract LINK variable name from element fields.
fn extract_link_name(fields: &Arc<BTreeMap<Arc<str>, DdValue>>) -> Option<String> {
    // Look for element.event.press containing LINK marker
    let element = fields.get("element")?;
    let event = element.get("event")?;

    // Check press event
    if let Some(press) = event.get("press") {
        // LINK is represented as Unit or Tagged{LINK}
        if matches!(press, DdValue::Unit) {
            // Look for link name in path or default
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
    _ctx: &GenericReactiveContext,
    _path: &str,
) -> RawElOrText {
    let placeholder_text = fields
        .get("placeholder")
        .and_then(|v| v.get("text"))
        .map(|v| v.to_display_string())
        .unwrap_or_default();

    let text_value = fields
        .get("text")
        .map(|v| v.to_display_string())
        .unwrap_or_default();

    let should_focus = fields
        .get("focus")
        .map(|v| v.is_truthy())
        .unwrap_or(false);

    let text_mutable = Mutable::new(text_value);
    let text_for_change = text_mutable.clone();

    // Build text input in one chain due to typestate pattern
    // Placeholder and focus settings use different type states
    match (placeholder_text.is_empty(), should_focus) {
        (true, true) => {
            TextInput::new()
                .label_hidden("text input")
                .text_signal(text_mutable.signal_cloned())
                .on_change(move |new_text| { text_for_change.set(new_text); })
                .focus(true)
                .unify()
        }
        (true, false) => {
            TextInput::new()
                .label_hidden("text input")
                .text_signal(text_mutable.signal_cloned())
                .on_change(move |new_text| { text_for_change.set(new_text); })
                .unify()
        }
        (false, true) => {
            TextInput::new()
                .label_hidden("text input")
                .text_signal(text_mutable.signal_cloned())
                .placeholder(Placeholder::new(&placeholder_text))
                .on_change(move |new_text| { text_for_change.set(new_text); })
                .focus(true)
                .unify()
        }
        (false, false) => {
            TextInput::new()
                .label_hidden("text input")
                .text_signal(text_mutable.signal_cloned())
                .placeholder(Placeholder::new(&placeholder_text))
                .on_change(move |new_text| { text_for_change.set(new_text); })
                .unify()
        }
    }
}

/// Render a checkbox element.
fn render_reactive_checkbox(
    fields: &Arc<BTreeMap<Arc<str>, DdValue>>,
    _ctx: &GenericReactiveContext,
    _path: &str,
) -> RawElOrText {
    let checked = fields
        .get("checked")
        .map(|v| v.is_truthy())
        .unwrap_or(false);

    let aria_checked = if checked { "true" } else { "false" };

    let el = El::new()
        .update_raw_el(move |raw_el| {
            raw_el
                .attr("role", "checkbox")
                .attr("aria-checked", aria_checked)
        });

    if checked {
        el.child(zoon::Text::new("✓")).unify()
    } else {
        el.unify()
    }
}

/// Render a label element.
fn render_reactive_label(
    fields: &Arc<BTreeMap<Arc<str>, DdValue>>,
    ctx: &GenericReactiveContext,
) -> RawElOrText {
    let text = fields
        .get("label")
        .map(|v| v.to_display_string())
        .unwrap_or_default();

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
fn render_reactive_paragraph(fields: &Arc<BTreeMap<Arc<str>, DdValue>>) -> RawElOrText {
    let text = fields
        .get("content")
        .map(|v| v.to_display_string())
        .unwrap_or_default();

    Paragraph::new().content(text).unify()
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
    let ctx_for_render = ctx.clone();
    let doc_for_render = document;

    // Get the render signal first (before moving ctx)
    let render_signal = ctx.render_signal();

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
