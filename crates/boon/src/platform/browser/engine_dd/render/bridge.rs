//! Value → Zoon element conversion.
//!
//! Takes DD Value descriptors (Element/button, Element/stripe, etc.)
//! and creates corresponding Zoon UI elements.

use std::sync::Arc;

use zoon::*;

use super::super::core::types::LinkId;
use super::super::io::general::{Event, GeneralHandle};
use super::super::io::worker::DdWorkerHandle;
use super::super::core::value::Value;

// ---------------------------------------------------------------------------
// Rendering with DD worker (SingleHold/LatestSum programs)
// ---------------------------------------------------------------------------

/// Render a DD document value as a Zoon element.
pub fn render_value(value: &Value, worker: &DdWorkerHandle) -> RawHtmlEl<web_sys::HtmlElement> {
    match value {
        Value::Number(n) => {
            let text = if n.0 == n.0.floor() && n.0.is_finite() {
                format!("{}", n.0 as i64)
            } else {
                format!("{}", n.0)
            };
            RawHtmlEl::new("span").child(text)
        }
        Value::Text(s) => RawHtmlEl::new("span").child(s.to_string()),
        Value::Tag(s) => {
            if s.as_ref() == "NoElement" {
                return RawHtmlEl::new("span");
            }
            RawHtmlEl::new("span").child(s.to_string())
        }
        Value::Bool(b) => {
            let text = if *b { "True" } else { "False" };
            RawHtmlEl::new("span").child(text)
        }
        Value::Tagged { tag, fields } => render_tagged(tag, fields, worker),
        Value::Object(fields) => {
            let el = RawHtmlEl::<web_sys::HtmlElement>::new("div");
            el.child(value.to_display_string())
        }
        Value::Unit => RawHtmlEl::new("span"),
    }
}

fn render_tagged(
    tag: &str,
    fields: &Arc<std::collections::BTreeMap<Arc<str>, Value>>,
    worker: &DdWorkerHandle,
) -> RawHtmlEl<web_sys::HtmlElement> {
    match tag {
        "ElementButton" => render_button(fields, worker),
        "ElementStripe" => render_stripe(fields, worker),
        "DocumentNew" => render_document(fields, worker),
        _ => {
            let text = format!("{}[...]", tag);
            RawHtmlEl::new("span").child(text)
        }
    }
}

fn render_button(
    fields: &std::collections::BTreeMap<Arc<str>, Value>,
    worker: &DdWorkerHandle,
) -> RawHtmlEl<web_sys::HtmlElement> {
    let label = fields
        .get("label" as &str)
        .map(|v| v.to_display_string())
        .unwrap_or_default();

    let link_id = fields
        .get("press_link" as &str)
        .and_then(|v| v.as_text())
        .map(|s| LinkId::new(s.to_string()));

    let button = RawHtmlEl::<web_sys::HtmlElement>::new("button")
        .attr("role", "button")
        .child(label);

    if let Some(link_id) = link_id {
        let worker_ref = worker.clone_ref();
        button.event_handler(move |_: events::Click| {
            worker_ref.inject_event(&link_id, Value::Unit);
        })
    } else {
        button
    }
}

fn render_stripe(
    fields: &std::collections::BTreeMap<Arc<str>, Value>,
    worker: &DdWorkerHandle,
) -> RawHtmlEl<web_sys::HtmlElement> {
    let direction = fields
        .get("direction" as &str)
        .and_then(|v| v.as_tag())
        .unwrap_or("Column");

    let el = RawHtmlEl::<web_sys::HtmlElement>::new("div").style("display", "flex");

    let el = if direction == "Row" {
        el.style("flex-direction", "row")
    } else {
        el.style("flex-direction", "column")
    };

    if let Some(Value::Tagged {
        tag,
        fields: list_fields,
    }) = fields.get("items" as &str)
    {
        if tag.as_ref() == "List" {
            let mut items: Vec<_> = list_fields.iter().collect();
            items.sort_by_key(|(k, _)| Arc::clone(k));
            let el = items.iter().fold(el, |el, (_key, item)| {
                el.child(render_value(item, worker))
            });
            return el;
        }
    }

    el
}

fn render_document(
    fields: &std::collections::BTreeMap<Arc<str>, Value>,
    worker: &DdWorkerHandle,
) -> RawHtmlEl<web_sys::HtmlElement> {
    if let Some(root) = fields.get("root" as &str) {
        render_value(root, worker)
    } else {
        RawHtmlEl::new("div").child("Empty document")
    }
}

// ---------------------------------------------------------------------------
// Rendering with general handle (General programs)
// ---------------------------------------------------------------------------

/// Render a DD document value with a GeneralHandle for event injection.
pub fn render_value_general(
    value: &Value,
    handle: &GeneralHandle,
) -> RawHtmlEl<web_sys::HtmlElement> {
    render_general(value, handle, "")
}

fn render_general(
    value: &Value,
    handle: &GeneralHandle,
    link_path: &str,
) -> RawHtmlEl<web_sys::HtmlElement> {
    match value {
        Value::Number(n) => {
            let text = if n.0 == n.0.floor() && n.0.is_finite() {
                format!("{}", n.0 as i64)
            } else {
                format!("{}", n.0)
            };
            RawHtmlEl::new("span").child(text)
        }
        Value::Text(s) => RawHtmlEl::new("span").child(s.to_string()),
        Value::Tag(s) => {
            if s.as_ref() == "NoElement" || s.as_ref() == "SKIP" {
                return RawHtmlEl::new("span");
            }
            RawHtmlEl::new("span").child(s.to_string())
        }
        Value::Bool(b) => {
            let text = if *b { "True" } else { "False" };
            RawHtmlEl::new("span").child(text)
        }
        Value::Tagged { tag, fields } => {
            render_tagged_general(tag, fields, handle, link_path)
        }
        Value::Object(_) => RawHtmlEl::new("span").child(value.to_display_string()),
        Value::Unit => RawHtmlEl::new("span"),
    }
}

fn render_tagged_general(
    tag: &str,
    fields: &Arc<std::collections::BTreeMap<Arc<str>, Value>>,
    handle: &GeneralHandle,
    link_path: &str,
) -> RawHtmlEl<web_sys::HtmlElement> {
    match tag {
        "ElementButton" => render_button_general(fields, handle, link_path),
        "ElementStripe" => render_stripe_general(fields, handle, link_path),
        "ElementContainer" => render_container_general(fields, handle, link_path),
        "ElementStack" => render_stack_general(fields, handle, link_path),
        "ElementLabel" => render_label_general(fields, handle, link_path),
        "ElementTextInput" => render_text_input_general(fields, handle, link_path),
        "ElementCheckbox" => render_checkbox_general(fields, handle, link_path),
        "ElementParagraph" => render_paragraph_general(fields, handle, link_path),
        "ElementLink" => render_link_general(fields, handle, link_path),
        "DocumentNew" => {
            if let Some(root) = fields.get("root" as &str) {
                render_general(root, handle, link_path)
            } else {
                // No root — render empty (e.g. timer hasn't fired yet)
                RawHtmlEl::new("span")
            }
        }
        _ => RawHtmlEl::new("span").child(format!("{}[...]", tag)),
    }
}

fn get_link_path_from_element(fields: &std::collections::BTreeMap<Arc<str>, Value>) -> String {
    // Extract link path from element field
    if let Some(element) = fields.get("element" as &str) {
        if let Some(link_path) = element.get_field("link_path") {
            if let Some(s) = link_path.as_text() {
                return s.to_string();
            }
        }
    }
    String::new()
}

fn render_button_general(
    fields: &std::collections::BTreeMap<Arc<str>, Value>,
    handle: &GeneralHandle,
    link_path: &str,
) -> RawHtmlEl<web_sys::HtmlElement> {
    let label = fields
        .get("label" as &str)
        .map(|v| v.to_display_string())
        .unwrap_or_default();

    // Extract link_path from element field or press_link
    let effective_link = fields
        .get("press_link" as &str)
        .and_then(|v| v.as_text())
        .map(|s| s.to_string())
        .unwrap_or_else(|| link_path.to_string());

    let mut button = RawHtmlEl::<web_sys::HtmlElement>::new("button")
        .attr("role", "button")
        .child(label);

    // Apply styles from the style field
    button = apply_styles(button, fields);

    if !effective_link.is_empty() {
        let handle_ref = handle.clone_ref();
        let link = effective_link.clone();
        let button = button.event_handler(move |_: events::Click| {
            handle_ref.inject_event(Event::LinkPress {
                link_path: link.clone(),
            });
        });

        // Handle hovered state
        let handle_hover_in = handle.clone_ref();
        let link_hover_in = effective_link.clone();
        let button = button.event_handler(move |_: events::MouseEnter| {
            handle_hover_in.inject_event(Event::HoverChange {
                link_path: link_hover_in.clone(),
                hovered: true,
            });
        });

        let handle_hover_out = handle.clone_ref();
        let link_hover_out = effective_link;
        button.event_handler(move |_: events::MouseLeave| {
            handle_hover_out.inject_event(Event::HoverChange {
                link_path: link_hover_out.clone(),
                hovered: false,
            });
        })
    } else {
        button
    }
}

fn render_stripe_general(
    fields: &std::collections::BTreeMap<Arc<str>, Value>,
    handle: &GeneralHandle,
    link_path: &str,
) -> RawHtmlEl<web_sys::HtmlElement> {
    let direction = fields
        .get("direction" as &str)
        .and_then(|v| v.as_tag())
        .unwrap_or("Column");

    let el = RawHtmlEl::<web_sys::HtmlElement>::new("div").style("display", "flex");
    let el = if direction == "Row" {
        el.style("flex-direction", "row")
    } else {
        el.style("flex-direction", "column")
    };
    let el = apply_styles(el, fields);

    let el = if let Some(Value::Tagged {
        tag,
        fields: list_fields,
    }) = fields.get("items" as &str)
    {
        if tag.as_ref() == "List" {
            let mut items: Vec<_> = list_fields.iter().collect();
            items.sort_by_key(|(k, _)| Arc::clone(k));
            items.iter().fold(el, |el, (_key, item)| {
                el.child(render_general(item, handle, link_path))
            })
        } else {
            el
        }
    } else {
        el
    };

    // Wire hover handlers — extract link path from element.hovered LINK field
    let hover_link = extract_hover_link_path(fields, link_path);
    if let Some(hover_link) = hover_link {
        let handle_hover_in = handle.clone_ref();
        let link_hover_in = hover_link.clone();
        let handle_hover_out = handle.clone_ref();
        let link_hover_out = hover_link;
        el.after_insert(move |el: web_sys::HtmlElement| {
            // Raw JS listeners — Zoon's events::MouseEnter doesn't fire for
            // synthetic mouseenter dispatched by the test infrastructure
            let link_in = link_hover_in.clone();
            let handle_in = handle_hover_in.clone_ref();
            let enter_closure = wasm_bindgen::closure::Closure::<dyn Fn()>::new(move || {
                handle_in.inject_event(Event::HoverChange {
                    link_path: link_in.clone(),
                    hovered: true,
                });
            });
            el.add_event_listener_with_callback(
                "mouseenter",
                enter_closure.as_ref().unchecked_ref(),
            )
            .ok();
            enter_closure.forget();

            let link_out = link_hover_out.clone();
            let handle_out = handle_hover_out.clone_ref();
            let leave_closure = wasm_bindgen::closure::Closure::<dyn Fn()>::new(move || {
                handle_out.inject_event(Event::HoverChange {
                    link_path: link_out.clone(),
                    hovered: false,
                });
            });
            el.add_event_listener_with_callback(
                "mouseleave",
                leave_closure.as_ref().unchecked_ref(),
            )
            .ok();
            leave_closure.forget();
        })
    } else {
        el
    }
}

fn render_container_general(
    fields: &std::collections::BTreeMap<Arc<str>, Value>,
    handle: &GeneralHandle,
    link_path: &str,
) -> RawHtmlEl<web_sys::HtmlElement> {
    let el = RawHtmlEl::<web_sys::HtmlElement>::new("div");
    let el = apply_styles(el, fields);
    if let Some(child) = fields.get("child" as &str) {
        el.child(render_general(child, handle, link_path))
    } else {
        el
    }
}

fn render_stack_general(
    fields: &std::collections::BTreeMap<Arc<str>, Value>,
    handle: &GeneralHandle,
    link_path: &str,
) -> RawHtmlEl<web_sys::HtmlElement> {
    let el = RawHtmlEl::<web_sys::HtmlElement>::new("div")
        .style("position", "relative");

    if let Some(Value::Tagged { tag, fields: list_fields }) = fields.get("layers" as &str) {
        if tag.as_ref() == "List" {
            let mut items: Vec<_> = list_fields.iter().collect();
            items.sort_by_key(|(k, _)| Arc::clone(k));
            return items.iter().fold(el, |el, (_key, item)| {
                el.child(render_general(item, handle, link_path))
            });
        }
    }
    el
}

fn render_label_general(
    fields: &std::collections::BTreeMap<Arc<str>, Value>,
    handle: &GeneralHandle,
    link_path: &str,
) -> RawHtmlEl<web_sys::HtmlElement> {
    let label = fields
        .get("label" as &str)
        .map(|v| v.to_display_string())
        .unwrap_or_default();

    let el = RawHtmlEl::<web_sys::HtmlElement>::new("span")
        .child(&label);
    let el = apply_styles(el, fields);

    // Wire up events (double_click, hover) from __link_path__
    let effective_link = fields
        .get("__link_path__" as &str)
        .and_then(|v| v.as_text())
        .map(|s| s.to_string())
        .unwrap_or_else(|| link_path.to_string());

    if !effective_link.is_empty() {
        let handle_dbl = handle.clone_ref();
        let link_dbl = effective_link.clone();
        let el = el.event_handler(move |_: events::DoubleClick| {
            handle_dbl.inject_event(Event::DoubleClick {
                link_path: link_dbl.clone(),
            });
        });

        let handle_hover_in = handle.clone_ref();
        let link_hover_in = effective_link.clone();
        let el = el.event_handler(move |_: events::MouseEnter| {
            handle_hover_in.inject_event(Event::HoverChange {
                link_path: link_hover_in.clone(),
                hovered: true,
            });
        });

        let handle_hover_out = handle.clone_ref();
        let link_hover_out = effective_link;
        el.event_handler(move |_: events::MouseLeave| {
            handle_hover_out.inject_event(Event::HoverChange {
                link_path: link_hover_out.clone(),
                hovered: false,
            });
        })
    } else {
        el
    }
}

fn render_text_input_general(
    fields: &std::collections::BTreeMap<Arc<str>, Value>,
    handle: &GeneralHandle,
    link_path: &str,
) -> RawHtmlEl<web_sys::HtmlElement> {
    let text = fields
        .get("text" as &str)
        .and_then(|v| v.as_text())
        .map(|s| s.to_string())
        .unwrap_or_default();

    let placeholder = fields
        .get("placeholder" as &str)
        .and_then(|v| v.get_field("text"))
        .and_then(|v| v.as_text())
        .map(|s| s.to_string())
        .unwrap_or_default();

    let focus = fields
        .get("focus" as &str)
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let input = RawHtmlEl::<web_sys::HtmlElement>::new("input")
        .attr("type", "text")
        .attr("value", &text)
        .attr("placeholder", &placeholder);

    let input = if focus {
        let text_for_insert = text.clone();
        let text_len: u32 = text.len().try_into().unwrap_or(0);
        input
            .attr("autofocus", "")
            .after_insert(move |el: web_sys::HtmlElement| {
                let input_el: web_sys::HtmlInputElement = el.unchecked_into();
                // Set DOM .value property directly (attr only sets default value)
                input_el.set_value(&text_for_insert);
                // Position cursor at end of text (autofocus handles focus itself)
                // Do NOT call .focus() here — it triggers blur→re-render→infinite loop
                input_el.set_selection_range(text_len, text_len).ok();
                // DEBUG: log the actual DOM value after setting
                let _ = js_sys::eval(&format!(
                    "window.__DD_INPUT_DOM_VALUE__ = '{}'; window.__DD_INPUT_SET_VALUE__ = '{}'",
                    input_el.value().replace('\'', "\\'"),
                    text_for_insert.replace('\'', "\\'")
                ));
            })
    } else {
        input
    };
    let input = apply_styles(input, fields);

    // Wire up events — check __link_path__ field first, fall back to parent link_path
    let effective_link = fields
        .get("__link_path__" as &str)
        .and_then(|v| v.as_text())
        .map(|s| s.to_string())
        .unwrap_or_else(|| link_path.to_string());

    if !effective_link.is_empty() {
        let handle_key = handle.clone_ref();
        let link_key = effective_link.clone();
        let input = input.event_handler(move |ev: events::KeyDown| {
            let key = ev.key();
            handle_key.inject_event(Event::KeyDown {
                link_path: link_key.clone(),
                key,
            });
        });

        let handle_input = handle.clone_ref();
        let link_input = effective_link.clone();
        let input = input.event_handler(move |ev: events::Input| {
            let target = ev.target().unwrap();
            let input_el: web_sys::HtmlInputElement = target.unchecked_into();
            let text = input_el.value();
            handle_input.inject_event(Event::TextChange {
                link_path: link_input.clone(),
                text,
            });
        });

        let handle_blur = handle.clone_ref();
        let link_blur = effective_link;
        input.event_handler(move |_: events::Blur| {
            handle_blur.inject_event(Event::Blur {
                link_path: link_blur.clone(),
            });
        })
    } else {
        input
    }
}

fn render_checkbox_general(
    fields: &std::collections::BTreeMap<Arc<str>, Value>,
    handle: &GeneralHandle,
    link_path: &str,
) -> RawHtmlEl<web_sys::HtmlElement> {
    let checked = fields
        .get("checked" as &str)
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let has_icon = fields.get("icon" as &str).is_some()
        && !matches!(fields.get("icon" as &str), Some(Value::Tag(t)) if t.as_ref() == "NoElement");

    let effective_link = fields
        .get("__link_path__" as &str)
        .and_then(|v| v.as_text())
        .map(|s| s.to_string())
        .unwrap_or_else(|| link_path.to_string());

    if has_icon {
        // Custom icon checkbox — render as div with role="checkbox"
        let icon_value = fields.get("icon" as &str).unwrap();
        let icon_el = render_value_general(icon_value, handle);

        let el = RawHtmlEl::<web_sys::HtmlElement>::new("div")
            .attr("role", "checkbox")
            .attr("aria-checked", if checked { "true" } else { "false" })
            .attr("data-link", &effective_link)
            .child(icon_el);

        if !effective_link.is_empty() {
            let handle_ref = handle.clone_ref();
            let link = effective_link;
            el.event_handler(move |_: events::Click| {
                handle_ref.inject_event(Event::LinkClick {
                    link_path: link.clone(),
                });
            })
        } else {
            el
        }
    } else {
        // Standard checkbox — render as <input type="checkbox">
        let input = RawHtmlEl::<web_sys::HtmlElement>::new("input")
            .attr("type", "checkbox")
            .attr("role", "checkbox");

        let input = if checked {
            input.attr("checked", "")
        } else {
            input
        };

        if !effective_link.is_empty() {
            let handle_ref = handle.clone_ref();
            let link = effective_link;
            input.event_handler(move |_: events::Click| {
                handle_ref.inject_event(Event::LinkClick {
                    link_path: link.clone(),
                });
            })
        } else {
            input
        }
    }
}

fn render_paragraph_general(
    fields: &std::collections::BTreeMap<Arc<str>, Value>,
    handle: &GeneralHandle,
    link_path: &str,
) -> RawHtmlEl<web_sys::HtmlElement> {
    let el = RawHtmlEl::<web_sys::HtmlElement>::new("p");

    if let Some(Value::Tagged {
        tag,
        fields: list_fields,
    }) = fields.get("contents" as &str)
    {
        if tag.as_ref() == "List" {
            let mut items: Vec<_> = list_fields.iter().collect();
            items.sort_by_key(|(k, _)| Arc::clone(k));
            return items.iter().fold(el, |el, (_key, item)| {
                el.child(render_general(item, handle, link_path))
            });
        }
    }

    el
}

fn render_link_general(
    fields: &std::collections::BTreeMap<Arc<str>, Value>,
    handle: &GeneralHandle,
    link_path: &str,
) -> RawHtmlEl<web_sys::HtmlElement> {
    let label = fields
        .get("label" as &str)
        .map(|v| v.to_display_string())
        .unwrap_or_default();

    let to = fields
        .get("to" as &str)
        .and_then(|v| v.as_text())
        .map(|s| s.to_string())
        .unwrap_or_default();

    let new_tab = fields.get("new_tab" as &str).is_some();

    let el = RawHtmlEl::<web_sys::HtmlElement>::new("a")
        .attr("href", &to)
        .child(label);

    if new_tab {
        el.attr("target", "_blank")
    } else {
        el
    }
}

// ---------------------------------------------------------------------------
// Static rendering (no event handlers)
// ---------------------------------------------------------------------------

pub fn render_value_static(value: &Value) -> RawHtmlEl<web_sys::HtmlElement> {
    match value {
        Value::Number(n) => {
            let text = if n.0 == n.0.floor() && n.0.is_finite() {
                format!("{}", n.0 as i64)
            } else {
                format!("{}", n.0)
            };
            RawHtmlEl::new("span").child(text)
        }
        Value::Text(s) => RawHtmlEl::new("span").child(s.to_string()),
        Value::Tag(s) => {
            if s.as_ref() == "NoElement" {
                return RawHtmlEl::new("span");
            }
            RawHtmlEl::new("span").child(s.to_string())
        }
        Value::Bool(b) => {
            let text = if *b { "True" } else { "False" };
            RawHtmlEl::new("span").child(text)
        }
        Value::Tagged { tag, fields } => render_tagged_static(tag, fields),
        Value::Object(_) => RawHtmlEl::new("span").child(value.to_display_string()),
        Value::Unit => RawHtmlEl::new("span"),
    }
}

fn render_tagged_static(
    tag: &str,
    fields: &Arc<std::collections::BTreeMap<Arc<str>, Value>>,
) -> RawHtmlEl<web_sys::HtmlElement> {
    match tag {
        "ElementButton" => {
            let label = fields
                .get("label" as &str)
                .map(|v| v.to_display_string())
                .unwrap_or_default();
            RawHtmlEl::<web_sys::HtmlElement>::new("button")
                .attr("role", "button")
                .child(label)
        }
        "ElementStripe" => render_stripe_static(fields),
        "ElementStack" => render_stack_static(fields),
        "ElementContainer" => render_container_static(fields),
        "ElementLabel" => {
            let label = fields
                .get("label" as &str)
                .map(|v| v.to_display_string())
                .unwrap_or_default();
            RawHtmlEl::new("span").child(label)
        }
        "ElementParagraph" => {
            let el = RawHtmlEl::<web_sys::HtmlElement>::new("p");
            if let Some(Value::Tagged { tag, fields: list_fields }) = fields.get("contents" as &str) {
                if tag.as_ref() == "List" {
                    let mut items: Vec<_> = list_fields.iter().collect();
                    items.sort_by_key(|(k, _)| Arc::clone(k));
                    return items.iter().fold(el, |el, (_key, item)| {
                        el.child(render_value_static(item))
                    });
                }
            }
            el
        }
        "DocumentNew" => {
            if let Some(root) = fields.get("root" as &str) {
                render_value_static(root)
            } else {
                RawHtmlEl::new("div").child("Empty document")
            }
        }
        _ => RawHtmlEl::new("span").child(format!("{}[...]", tag)),
    }
}

fn render_stripe_static(
    fields: &std::collections::BTreeMap<Arc<str>, Value>,
) -> RawHtmlEl<web_sys::HtmlElement> {
    let direction = fields
        .get("direction" as &str)
        .and_then(|v| v.as_tag())
        .unwrap_or("Column");

    let el = RawHtmlEl::<web_sys::HtmlElement>::new("div").style("display", "flex");
    let el = if direction == "Row" {
        el.style("flex-direction", "row")
    } else {
        el.style("flex-direction", "column")
    };

    if let Some(Value::Tagged { tag, fields: list_fields }) = fields.get("items" as &str) {
        if tag.as_ref() == "List" {
            let mut items: Vec<_> = list_fields.iter().collect();
            items.sort_by_key(|(k, _)| Arc::clone(k));
            return items.iter().fold(el, |el, (_key, item)| {
                el.child(render_value_static(item))
            });
        }
    }
    el
}

fn render_stack_static(
    fields: &std::collections::BTreeMap<Arc<str>, Value>,
) -> RawHtmlEl<web_sys::HtmlElement> {
    let el = RawHtmlEl::<web_sys::HtmlElement>::new("div")
        .style("position", "relative");

    if let Some(Value::Tagged { tag, fields: list_fields }) = fields.get("layers" as &str) {
        if tag.as_ref() == "List" {
            let mut items: Vec<_> = list_fields.iter().collect();
            items.sort_by_key(|(k, _)| Arc::clone(k));
            return items.iter().fold(el, |el, (_key, item)| {
                el.child(render_value_static(item))
            });
        }
    }
    el
}

fn render_container_static(
    fields: &std::collections::BTreeMap<Arc<str>, Value>,
) -> RawHtmlEl<web_sys::HtmlElement> {
    let el = RawHtmlEl::<web_sys::HtmlElement>::new("div");
    if let Some(child) = fields.get("child" as &str) {
        el.child(render_value_static(child))
    } else {
        el
    }
}

// ---------------------------------------------------------------------------
// Style helpers
// ---------------------------------------------------------------------------

/// Convert an Oklch tagged value to a CSS `oklch()` string.
fn oklch_to_css(fields: &std::collections::BTreeMap<Arc<str>, Value>) -> Option<String> {
    let l = fields.get("lightness" as &str)?.as_number()?;
    let c = fields.get("chroma" as &str)?.as_number()?;
    let h = fields.get("hue" as &str)?.as_number()?;
    let a = fields.get("alpha" as &str).and_then(|v| v.as_number());

    if let Some(alpha) = a {
        Some(format!("oklch({} {} {} / {})", l, c, h, alpha))
    } else {
        Some(format!("oklch({} {} {})", l, c, h))
    }
}

/// Extract hover link path from element's `element: [hovered: LINK]` field.
/// Returns `Some(path)` if the element has a hovered LINK with a `__path__`.
fn extract_hover_link_path(
    fields: &std::collections::BTreeMap<Arc<str>, Value>,
    parent_link_path: &str,
) -> Option<String> {
    // First check __link_path__ (set by |> LINK { alias } pipe)
    if let Some(path) = fields.get("__link_path__" as &str).and_then(|v| v.as_text()) {
        if !path.is_empty() {
            return Some(path.to_string());
        }
    }
    // Then check element.hovered.__path__ (set by element: [hovered: LINK])
    // The __path__ includes ".hovered" suffix from nested object evaluation,
    // but try_link_field_access looks up the base prefix (without ".hovered").
    // Strip the ".hovered" suffix to match.
    if let Some(element_val) = fields.get("element" as &str) {
        let hovered_val = match element_val {
            Value::Object(obj) => obj.get("hovered" as &str),
            _ => None,
        };
        if let Some(Value::Tagged { tag, fields: link_fields }) = hovered_val {
            if tag.as_ref() == "LINK" {
                if let Some(path) = link_fields.get("__path__" as &str).and_then(|v| v.as_text()) {
                    if !path.is_empty() {
                        // Strip ".hovered" suffix — the interpreter stores hover state
                        // under the base element prefix, not the field-qualified path
                        let base_path = path.strip_suffix(".hovered").unwrap_or(path);
                        return Some(base_path.to_string());
                    }
                }
            }
        }
    }
    // Fall back to parent link path if non-empty
    if !parent_link_path.is_empty() {
        Some(parent_link_path.to_string())
    } else {
        None
    }
}

/// Apply style fields (outline, padding, rounded_corners) from an element's fields.
fn apply_styles(
    el: RawHtmlEl<web_sys::HtmlElement>,
    fields: &std::collections::BTreeMap<Arc<str>, Value>,
) -> RawHtmlEl<web_sys::HtmlElement> {
    let style = match fields.get("style" as &str) {
        Some(Value::Object(obj)) => obj,
        _ => return el,
    };

    let mut el = el;

    // Outline
    if let Some(outline_val) = style.get("outline" as &str) {
        el = apply_outline(el, outline_val);
    }

    // Padding
    if let Some(Value::Object(padding)) = style.get("padding" as &str) {
        if let Some(row) = padding.get("row" as &str).and_then(|v| v.as_number()) {
            el = el
                .style("padding-left", &format!("{}px", row))
                .style("padding-right", &format!("{}px", row));
        }
        if let Some(col) = padding.get("column" as &str).and_then(|v| v.as_number()) {
            el = el
                .style("padding-top", &format!("{}px", col))
                .style("padding-bottom", &format!("{}px", col));
        }
    }

    // Rounded corners
    if let Some(rc) = style.get("rounded_corners" as &str).and_then(|v| v.as_number()) {
        el = el.style("border-radius", &format!("{}px", rc));
    }

    // Size (sets both width and height)
    if let Some(size) = style.get("size" as &str).and_then(|v| v.as_number()) {
        el = el
            .style("width", &format!("{}px", size))
            .style("height", &format!("{}px", size));
    }

    // Width
    if let Some(width) = style.get("width" as &str) {
        if let Some(n) = width.as_number() {
            el = el.style("width", &format!("{}px", n));
        } else if let Some(tag) = width.as_tag() {
            if tag == "Fill" {
                el = el.style("flex", "1").style("min-width", "0");
            }
        }
    }

    // Height
    if let Some(height) = style.get("height" as &str) {
        if let Some(n) = height.as_number() {
            el = el.style("height", &format!("{}px", n));
        } else if let Some(tag) = height.as_tag() {
            if tag == "Fill" {
                el = el.style("flex", "1").style("min-height", "0");
            }
        }
    }

    // Background
    if let Some(Value::Object(bg)) = style.get("background" as &str) {
        // background color: Oklch[...]
        if let Some(Value::Tagged { tag, fields: oklch_fields }) = bg.get("color" as &str) {
            if tag.as_ref() == "Oklch" {
                if let Some(css) = oklch_to_css(oklch_fields) {
                    el = el.style("background-color", &css);
                }
            }
        }
        // background url
        if let Some(url) = bg.get("url" as &str).and_then(|v| v.as_text()) {
            el = el.style("background-image", &format!("url(\"{}\")", url));
            el = el.style("background-repeat", "no-repeat");
            el = el.style("background-position", "center");
        }
    }

    // Font
    if let Some(Value::Object(font)) = style.get("font" as &str) {
        if let Some(size) = font.get("size" as &str).and_then(|v| v.as_number()) {
            el = el.style("font-size", &format!("{}px", size));
        }
        if let Some(Value::Tagged { tag, fields: color_fields }) = font.get("color" as &str) {
            if tag.as_ref() == "Oklch" {
                if let Some(css) = oklch_to_css(color_fields) {
                    el = el.style("color", &css);
                }
            }
        }
        // Font line decorations (strikethrough)
        if let Some(Value::Object(line)) = font.get("line" as &str) {
            if let Some(st) = line.get("strikethrough" as &str).and_then(|v| v.as_bool()) {
                if st {
                    el = el.style("text-decoration", "line-through");
                }
            }
        }
    }

    // Transform
    if let Some(Value::Object(transform)) = style.get("transform" as &str) {
        if let Some(rotate) = transform.get("rotate" as &str).and_then(|v| v.as_number()) {
            el = el.style("transform", &format!("rotate({}deg)", rotate));
        }
    }

    el
}

/// Apply outline from a Boon outline value to an HTML element.
fn apply_outline(
    el: RawHtmlEl<web_sys::HtmlElement>,
    outline: &Value,
) -> RawHtmlEl<web_sys::HtmlElement> {
    match outline {
        // NoOutline tag — no outline
        Value::Tag(t) if t.as_ref() == "NoOutline" => el,

        // Object with side + color
        Value::Object(obj) => {
            let color_css = match obj.get("color" as &str) {
                Some(Value::Tagged { tag, fields }) if tag.as_ref() == "Oklch" => {
                    oklch_to_css(fields)
                }
                _ => None,
            };

            if let Some(css) = color_css {
                let side = obj
                    .get("side" as &str)
                    .and_then(|v| v.as_tag())
                    .unwrap_or("Outer");

                let el = el.style("outline", &format!("1px solid {}", css));
                if side == "Inner" {
                    el.style("outline-offset", "-1px")
                } else {
                    el
                }
            } else {
                el
            }
        }

        _ => el,
    }
}
