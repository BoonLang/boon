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

use super::super::io::{fire_global_link, fire_global_link_with_bool, fire_global_link_with_text, fire_global_blur, fire_global_key_down, cell_signal, list_signal_vec, get_cell_value};
// Phase 7: toggle_cell_bool, update_cell_no_persist removed - runtime updates flow through DD
// Phase 7: find_template_hover_link, find_template_hover_cell removed - symbolic refs eliminated
// Phase 11b: cell_states_signal REMOVED - was broadcast anti-pattern causing spurious re-renders
// Cleanup: removed unused imports (cells_signal, add_dynamic_link_action, DynamicLinkAction, sync_cell_from_dd, AtomicU32, Ordering)

/// Helper function to get the variant name of a Value for debug logging.
/// Phase 7: Only pure DD value types - no symbolic references.
fn dd_value_variant_name(value: &Value) -> &'static str {
    match value {
        Value::Unit => "Unit",
        Value::Bool(_) => "Bool",
        Value::Number(_) => "Number",
        Value::Text(_) => "Text",
        Value::List(_) => "List",
        Value::Collection(_) => "Collection",
        Value::Object(_) => "Object",
        Value::Tagged { .. } => "Tagged",
        Value::CellRef(_) => "CellRef",
        Value::LinkRef(_) => "LinkRef",
        Value::TimerRef { .. } => "TimerRef",
        Value::Placeholder => "Placeholder",
        Value::Flushed(_) => "Flushed",
    }
}

// Phase 6: Removed dead code - extract_cell_ids() and extract_cell_ids_from_parts()
// These were unused after Phase 12 refactoring to use targeted multi-cell signals.

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

// ═══════════════════════════════════════════════════════════════════════════
// Phase 7: REMOVED FUNCTIONS
//   - evaluate_while_ref_now() - WhileRef is removed, DD computes arm selection
//   - evaluate_dd_value_for_filter() - Filtering happens in DD operators
//
// Pure DD: All computation happens in DD dataflow, not at render time.
// The bridge only renders pure data values from DD output streams.
// ═══════════════════════════════════════════════════════════════════════════

/// Resolve a CellRef to its actual value for rendering.
/// Phase 7: Only resolves CellRef - no WhileRef or ComputedRef.
fn resolve_cell_ref(value: &Value, states: &std::collections::HashMap<String, Value>) -> Value {
    match value {
        Value::CellRef(cell_id) => {
            states.get(&cell_id.name()).cloned().unwrap_or(Value::Unit)
        }
        other => other.clone(),
    }
}

/// Convert a Value Oklch color to CSS color string.
/// Phase 7: Pure DD - only handles Number values, no WhileRef evaluation.
/// Reactive color changes come from DD output streams, not render-time evaluation.
fn dd_oklch_to_css(value: &Value) -> Option<String> {
    match value {
        Value::Tagged { tag, fields } if tag.as_ref() == "Oklch" => {
            // Phase 7: Only handle Number values - DD computes reactive colors
            let lightness = fields.get("lightness")
                .and_then(|v| if let Value::Number(n) = v { Some(n.0) } else { None })
                .unwrap_or(0.5);
            let chroma = fields.get("chroma")
                .and_then(|v| if let Value::Number(n) = v { Some(n.0) } else { None })
                .unwrap_or(0.0);
            let hue = fields.get("hue")
                .and_then(|v| if let Value::Number(n) = v { Some(n.0) } else { None })
                .unwrap_or(0.0);
            let alpha = fields.get("alpha")
                .and_then(|v| if let Value::Number(n) = v { Some(n.0) } else { None });

            // oklch(lightness% chroma hue / alpha)
            if let Some(a) = alpha {
                if a == 0.0 {
                    None  // alpha=0 means invisible
                } else {
                    Some(format!("oklch({}% {} {} / {})", lightness * 100.0, chroma, hue, a))
                }
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

        // Phase 7: ReactiveText REMOVED - DD produces Text values directly
        // Text interpolation is computed in DD, not at render time

        Value::Tagged { tag, fields } => {
            zoon::println!("[DD render_dd_value] Tagged(tag='{}', fields={:?})", tag, fields.keys().collect::<Vec<_>>());
            render_tagged_element(tag.as_ref(), fields)
        }

        Value::CellRef(name) => {
            // CellRef is a reactive reference to a HOLD value
            // Phase 12: Use granular cell_signal() instead of coarse cell_states_signal()
            // This only fires when THIS specific cell changes, not when ANY cell changes
            let cell_id = name.to_string();

            // Phase 12: Check if this cell contains a list for incremental rendering
            let is_list_cell = get_cell_value(&cell_id)
                .map(|v| matches!(v, Value::List(_) | Value::Collection(_)))
                .unwrap_or(false);

            if is_list_cell {
                // Use incremental list rendering via VecDiff
                // children_signal_vec() only updates changed elements (O(delta))
                Column::new()
                    .items_signal_vec(
                        list_signal_vec(cell_id)  // Pass owned String
                            .map(|item| render_dd_value(&item))
                    )
                    .unify()
            } else {
                // Use scalar rendering for non-list cells
                El::new()
                    .child_signal(
                        cell_signal(cell_id)  // Pass owned String
                            .map(|value| {
                                let text = value
                                    .map(|v| v.to_display_string())
                                    .unwrap_or_else(|| "?".to_string());
                                Text::new(text)
                            })
                    )
                    .unify()
            }
        }

        Value::LinkRef(link_id) => {
            // LinkRef is a placeholder for an event source
            // In static rendering, show as unit (events are wired at button level)
            El::new().unify()
        }

        Value::TimerRef { id, interval_ms: _ } => {
            // TimerRef represents a timer-driven HOLD accumulator
            // The `id` is the HOLD id - render its reactive value
            // Phase 12: Use granular cell_signal() - only fires when this timer's cell changes
            let cell_id = id.to_string();

            // Create reactive element that updates only when this timer cell changes
            // NOTE: Returns empty string if HOLD hasn't been set yet (timer not fired)
            El::new()
                .child_signal(
                    cell_signal(cell_id)  // Pass owned String
                        .map(|value| {
                            let text = value
                                .map(|v| v.to_display_string())
                                .unwrap_or_default(); // Empty until first timer tick
                            Text::new(text)
                        })
                )
                .unify()
        }

        // ═══════════════════════════════════════════════════════════════════════════
        // Phase 7: SYMBOLIC REFERENCE VARIANTS REMOVED
        //
        // The following variants no longer exist in the pure DD Value enum:
        //   - WhileRef, ComputedRef, FilteredListRef, ReactiveFilteredList
        //   - FilteredListRefWithPredicate, MappedListRef, FilteredMappedListRef
        //   - FilteredMappedListWithPredicate, LatestRef, ReactiveText
        //   - PlaceholderField, PlaceholderWhileRef, NegatedPlaceholderField
        //
        // In pure DD:
        //   - All computation happens in DD dataflow, not at render time
        //   - Lists are rendered as Collection with children_signal_vec()
        //   - Reactive values flow through DD output streams
        //   - The bridge only renders pure data values
        // ═══════════════════════════════════════════════════════════════════════════

        Value::Placeholder => {
            // Placeholder should never be rendered directly - it's a DD map template marker
            Text::new("[placeholder]").unify()
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
    // Phase 7: DD computes reactive styling - bridge receives final values
    let style_value = fields.get("style");
    let outline_value = style_value.and_then(|s| s.get("outline"));

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
    } else if outline_value.is_some() {
        // Phase 7: Outline was specified but didn't match any pattern - apply no-outline
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

    // ═══════════════════════════════════════════════════════════════════════════
    // Phase 7: SYMBOLIC LIST REFS REMOVED
    //
    // MappedListRef, FilteredMappedListRef, FilteredMappedListWithPredicate
    // are removed. In pure DD:
    //   - List rendering uses Collection + children_signal_vec()
    //   - Filtering and mapping happen in DD operators
    //   - The bridge renders pre-computed Collection values
    // ═══════════════════════════════════════════════════════════════════════════

    // Phase 2.3: Check if items is a CellRef (cell-backed list) for reactive rendering
    let items_hold_ref = fields.get("items").and_then(|v| match v {
        Value::CellRef(cell_id) => Some(cell_id.name().to_string()),
        _ => None,
    });

    let items: Vec<RawElOrText> = if items_hold_ref.is_none() {
        // Static items - render directly
        fields
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
            .unwrap_or_default()
    } else {
        // Reactive items handled below via items_signal_vec
        Vec::new()
    };

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

    // Phase 2.3: Reactive rendering for HoldRef-backed items
    if direction == "Row" {
        if let Some(ref cell_id) = items_hold_ref {
            // Reactive Row with items_signal_vec
            zoon::println!("[DD render_stripe] Row with reactive items from {}", cell_id);
            let mut row = Row::new()
                .s(Gap::new().x(gap))
                .items_signal_vec(
                    list_signal_vec(cell_id.clone())
                        .map(|item| render_dd_value(&item))
                );
            // Apply styles
            if width_fill { row = row.s(zoon::Width::fill()); }
            if let Some(color) = bg_color { row = row.s(zoon::Background::new().color(color)); }
            if font_size.is_some() || font_color.is_some() {
                let mut font = zoon::Font::new();
                if let Some(size) = font_size { font = font.size(size); }
                if let Some(ref color) = font_color { font = font.color(color.clone()); }
                row = row.s(font);
            }
            if padding_x.is_some() || padding_y.is_some() {
                let mut padding = zoon::Padding::new();
                if let Some(x) = padding_x { padding = padding.x(x); }
                if let Some(y) = padding_y { padding = padding.y(y); }
                row = row.s(padding);
            }
            if let Some(link_id) = hovered_link_id {
                return row.on_hovered_change(move |is_hovered| {
                    fire_global_link_with_bool(&link_id, is_hovered);
                }).unify();
            }
            return row.unify();
        }
        // Static Row with items Vec
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
        if let Some(ref cell_id) = items_hold_ref {
            // Reactive Column with items_signal_vec
            zoon::println!("[DD render_stripe] Column with reactive items from {}", cell_id);
            let mut column = Column::new()
                .s(Gap::new().y(gap))
                .items_signal_vec(
                    list_signal_vec(cell_id.clone())
                        .map(|item| render_dd_value(&item))
                );
            // Apply styles
            if width_fill { column = column.s(zoon::Width::fill()); }
            if let Some(color) = bg_color { column = column.s(zoon::Background::new().color(color)); }
            if font_size.is_some() || font_color.is_some() {
                let mut font = zoon::Font::new();
                if let Some(size) = font_size { font = font.size(size); }
                if let Some(ref color) = font_color { font = font.color(color.clone()); }
                column = column.s(font);
            }
            if padding_x.is_some() || padding_y.is_some() {
                let mut padding = zoon::Padding::new();
                if let Some(x) = padding_x { padding = padding.x(x); }
                if let Some(y) = padding_y { padding = padding.y(y); }
                column = column.s(padding);
            }
            if let Some(link_id) = hovered_link_id {
                return column.on_hovered_change(move |is_hovered| {
                    fire_global_link_with_bool(&link_id, is_hovered);
                }).unify();
            }
            return column.unify();
        }
        // Static Column with items Vec
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
/// Phase 6: Added CellRef support for reactive layers via layers_signal_vec.
fn render_stack(fields: &Arc<std::collections::BTreeMap<Arc<str>, Value>>) -> RawElOrText {
    // Check if layers is a CellRef for reactive rendering
    if let Some(Value::CellRef(cell_id)) = fields.get("layers") {
        // Reactive path - use layers_signal_vec for O(delta) updates
        let cell_id_str = cell_id.name();
        zoon::println!("[DD render_stack] Reactive layers from CellRef '{}'", cell_id_str);
        return Stack::new()
            .layers_signal_vec(
                list_signal_vec(cell_id_str)
                    .map(|item| render_dd_value(&item))
            )
            .unify();
    }

    // Static path - materialize layers once
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

    // Phase 7: Font color is now computed by DD - bridge receives final values
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

    // Apply font styling (pure DD - no reactive WhileRef evaluation)
    let font_color_css = font_color_value.as_ref().and_then(|c| dd_oklch_to_css(c));
    let base = if font_size.is_some() || font_color_css.is_some() {
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
                                        super::super::io::update_cell_no_persist(cell_id, Value::text(""));
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
                                    super::super::io::update_cell_no_persist(cell_id, Value::text(""));
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

        // Phase 12: Use granular cell_signal() for checkbox - only updates when this cell changes
        let checkbox = Checkbox::new()
            .label_hidden("checkbox")
            .checked_signal(
                cell_signal(cell_id_for_signal)  // Pass owned String
                    .map(|value| {
                        value
                            .map(|v| match v {
                                Value::Bool(b) => b,
                                Value::Tagged { tag, .. } => BoolTag::is_true(tag.as_ref()),
                                _ => false,
                            })
                            .unwrap_or(false)
                    })
            )
            .icon({
                // Phase 12: Use granular cell_signal() for icon - only updates when this cell changes
                move |_checked_mutable| {
                    El::new()
                        .s(zoon::Width::exact(40))
                        .s(zoon::Height::exact(40))
                        .update_raw_el(|raw_el| raw_el.style("pointer-events", "none"))
                        .s(zoon::Background::new().url_signal(
                            cell_signal(cell_id_for_icon.clone())  // Clone and pass owned
                                .map(|value| {
                                    let checked = value
                                        .map(|v| match v {
                                            Value::Bool(b) => b,
                                            Value::Tagged { tag, .. } => BoolTag::is_true(tag.as_ref()),
                                            _ => false,
                                        })
                                        .unwrap_or(false);
                                    if checked { CHECKED_SVG } else { UNCHECKED_SVG }
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
            // No link, toggle via DD event system (Phase 7: toggle_cell_bool was removed)
            let cell_id_clone = cell_id_for_toggle.clone();
            return checkbox
                .update_raw_el(move |raw_el| {
                    let cell_id = cell_id_clone.clone();
                    raw_el.event_handler(move |_: zoon::events::Click| {
                        zoon::println!("[DD CHECKBOX CLICK] RAW event handler (no link_id), toggling {}", cell_id);
                        // Fire toggle event through DD - runtime updates flow through DD, not direct mutation
                        fire_global_link_with_text(
                            "dd_cell_update",
                            &format!("bool_toggle:{}", cell_id)
                        );
                    })
                })
                .unify();
        }
    }

    // Phase 7: ComputedRef REMOVED - DD computes checkbox state directly
    // The bridge receives final Bool values, not deferred computations

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
            // Phase 12: Use granular cell_signal() for label - only updates when this cell changes
            let cell_id = name.to_string();
            Label::new()
                .label_signal(
                    cell_signal(cell_id)  // Pass owned String directly
                        .map(|value| {
                            value
                                .map(|v| v.to_display_string())
                                .unwrap_or_default()
                        })
                )
                .for_input("dd_text_input")
        }
        // Phase 7: ReactiveText REMOVED - DD produces Text values directly
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
/// Phase 6: Added CellRef support for reactive text via content_signal.
fn render_paragraph(fields: &Arc<std::collections::BTreeMap<Arc<str>, Value>>) -> RawElOrText {
    // Check if contents/content is a CellRef for reactive rendering
    if let Some(Value::CellRef(cell_id)) = fields.get("contents").or_else(|| fields.get("content")) {
        // Reactive path - use content_signal for O(delta) updates
        let cell_id_str = cell_id.name();
        zoon::println!("[DD render_paragraph] Reactive content from CellRef '{}'", cell_id_str);
        return Paragraph::new()
            .content_signal(
                cell_signal(cell_id_str)
                    .map(|value| {
                        value
                            .map(|v| extract_text_content(&v))
                            .unwrap_or_default()
                    })
            )
            .unify();
    }

    // Static path - extract content once
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
