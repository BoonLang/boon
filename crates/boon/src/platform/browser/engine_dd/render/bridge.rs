//! DD Bridge - Converts DD values to Zoon elements.
//!
//! This module provides functions to render `Value` as Zoon elements.
//! Currently implements static rendering; reactive rendering will use
//! Output streams in a future phase.

use std::sync::Arc;

use super::super::eval::interpreter::{DdContext, DdResult};
use super::super::core::types::{BoolTag, ElementTag, Key as DdKey};
use super::super::core::value::{Value, WhileArm};
use zoon::*;

use super::super::io::{fire_global_link, fire_global_link_with_text, fire_global_link_with_bool, fire_global_blur, fire_global_key_down, cell_signal, list_signal_vec, is_list_cell};
// Phase 7: toggle_cell_bool removed - runtime updates flow through DD
// Phase 7: find_template_hover_link, find_template_hover_cell removed - symbolic refs eliminated
// Phase 11b: cell_states_signal REMOVED - was broadcast anti-pattern causing spurious re-renders
// Cleanup: removed unused imports (cells_signal, sync_cell_from_dd, AtomicU32, Ordering)

/// Helper function to get the variant name of a Value for debug logging.
/// Phase 7: Only pure DD value types - no symbolic references.
fn dd_value_variant_name(value: &Value) -> &'static str {
    match value {
        Value::Unit => "Unit",
        Value::Bool(_) => "Bool",
        Value::Number(_) => "Number",
        Value::Text(_) => "Text",
        Value::List(_) => "List",
        Value::Object(_) => "Object",
        Value::Tagged { .. } => "Tagged",
        Value::CellRef(_) => "CellRef",
        Value::LinkRef(_) => "LinkRef",
        Value::TimerRef { .. } => "TimerRef",
        Value::Placeholder => "Placeholder",
        Value::PlaceholderField(_) => "PlaceholderField",
        Value::WhileConfig(_) => "WhileConfig",
        Value::PlaceholderWhile(_) => "PlaceholderWhile",
        Value::Flushed(_) => "Flushed",
    }
}

// Phase 6: Removed dead code - extract_cell_ids() and extract_cell_ids_from_parts()
// These were unused after Phase 12 refactoring to use targeted multi-cell signals.

/// Get the current value of the focused text input via DOM access.
/// This is used when Enter is pressed to capture the input text.
/// Pure DD: fail fast if there is no active input element.
#[cfg(target_arch = "wasm32")]
fn get_dd_text_input_value() -> String {
    use zoon::*;

    let active = document().active_element();
    let active_tag = active.as_ref().map(|el| el.tag_name()).unwrap_or_default();
    let input = active.and_then(|el| el.dyn_into::<web_sys::HtmlInputElement>().ok())
        .unwrap_or_else(|| {
            panic!(
                "[DD TextInput] Unable to read input value: active element is not an input (tag='{}')",
                active_tag
            );
        });
    let value = input.value();
    zoon::println!("[DD TextInput] get_dd_text_input_value: active_tag={}, value='{}'", active_tag, value);
    value
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

/// Convert a Value Oklch color to CSS color string.
/// Phase 7: Pure DD - only handles Number values, no WhileRef evaluation.
/// Reactive color changes come from DD output streams, not render-time evaluation.
fn dd_oklch_to_css(value: &Value) -> Option<String> {
    match value {
        Value::Tagged { tag, fields } if tag.as_ref() == "Oklch" => {
            // Phase 7: Only handle Number values - DD computes reactive colors
            let lightness = fields.get("lightness")
                .and_then(|v| if let Value::Number(n) = v { Some(n.0) } else { None })
                .unwrap_or_else(|| {
                    panic!("[DD Render] Oklch missing numeric 'lightness'");
                });
            let chroma = fields.get("chroma")
                .and_then(|v| if let Value::Number(n) = v { Some(n.0) } else { None })
                .unwrap_or_else(|| {
                    panic!("[DD Render] Oklch missing numeric 'chroma'");
                });
            let hue = fields.get("hue")
                .and_then(|v| if let Value::Number(n) = v { Some(n.0) } else { None })
                .unwrap_or_else(|| {
                    panic!("[DD Render] Oklch missing numeric 'hue'");
                });
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
        None => panic!("[DD Render] No document produced"),
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

        Value::List(handle) => {
            let cell_id = handle.cell_id
                .as_deref()
                .map(str::to_string)
                .unwrap_or_else(|| handle.id.to_string());
            Column::new()
                .items_signal_vec(
                    list_signal_vec(cell_id.to_string())
                        .map(|item| render_dd_value(&item))
                )
                .unify()
        }

        Value::WhileConfig(config) => {
            let cell_id = config.cell_id.name().to_string();
            let arms = config.arms.clone();
            let default = config.default.clone();
            let cell_id_for_signal = cell_id.clone();
            El::new()
                .child_signal(
                    cell_signal(cell_id)
                        .map(move |value| {
                            let value = value.unwrap_or_else(|| {
                                panic!("[DD Render] Missing cell value for '{}'", cell_id_for_signal);
                            });
                            let selected = select_while_arm(&value, &arms, &default);
                            render_dd_value(&selected)
                        })
                )
                .unify()
        }
        Value::PlaceholderWhile(_) => {
            panic!("[DD Render] Placeholder WHILE reached render; template substitution failed");
        }
        Value::Object(fields) => {
            // Render object as debug representation
            let debug = fields
                .iter()
                .filter(|(k, _)| k.as_ref() != "__key")
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
            if is_list_cell(&cell_id) {
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
                let cell_id_for_signal = cell_id.clone();
                El::new()
                    .child_signal(
                        cell_signal(cell_id)  // Pass owned String
                            .map(move |value| {
                                let value = value.unwrap_or_else(|| {
                                    panic!("[DD Render] Missing cell value for '{}'", cell_id_for_signal);
                                });
                                Text::new(value.to_display_string())
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
            let cell_id_for_signal = cell_id.clone();
            El::new()
                .child_signal(
                    cell_signal(cell_id)  // Pass owned String
                        .map(move |value| {
                            let value = value.unwrap_or_else(|| {
                                panic!("[DD Render] Missing timer cell value for '{}'", cell_id_for_signal);
                            });
                            Text::new(value.to_display_string())
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
            panic!("[DD Render] Placeholder reached render; DD map substitution failed");
        }
        Value::PlaceholderField(_) => {
            panic!("[DD Render] PlaceholderField reached render; template substitution failed");
        }
        Value::Flushed(_) => {
            panic!("[DD Render] Flushed value reached render; missing FLUSH handler");
        }
    }
}

/// Render a tagged object as a Zoon element.
fn render_tagged_element(tag: &str, fields: &Arc<std::collections::BTreeMap<Arc<str>, Value>>) -> RawElOrText {
    zoon::println!("[DD render_tagged] tag='{}', fields={:?}", tag, fields.keys().collect::<Vec<_>>());
    if BoolTag::is_bool_tag(tag) {
        return Text::new(tag).unify();
    }
    match tag {
        "Element" => render_element(fields),
        "NoElement" => El::new().unify(),
        _ => {
            panic!("[DD render_tagged] Unknown tag '{}'", tag);
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
        .unwrap_or_else(|| {
            panic!("[DD render_element] Missing required '_element_type' field");
        });

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
            panic!("[DD render_element] Unknown element type '{}'", element_type);
        }
    }
}

/// Render a button element.
fn render_button(fields: &Arc<std::collections::BTreeMap<Arc<str>, Value>>) -> RawElOrText {
    let label = fields
        .get("label")
        .map(|v| v.to_display_string())
        .unwrap_or_else(|| {
            panic!("[DD render_button] Missing required 'label' field");
        });

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

    let outline_opt: Option<Outline> = match outline_value {
        None => None,
        Some(Value::Tagged { tag, .. }) if tag.as_ref() == "NoOutline" => None,
        Some(Value::Object(obj)) => {
            let color = obj.get("color").and_then(|c| dd_oklch_to_css(c)).unwrap_or_else(|| {
                panic!("[DD render_button] outline.color must be Oklch");
            });
            let is_inner = match obj.get("side") {
                None => false,
                Some(Value::Tagged { tag, .. }) if tag.as_ref() == "Inner" => true,
                Some(Value::Tagged { tag, .. }) if tag.as_ref() == "Outer" => false,
                Some(other) => {
                    panic!("[DD render_button] outline.side must be Inner/Outer, found {:?}", other);
                }
            };
            let width = match obj.get("width") {
                None => 1,
                Some(Value::Number(n)) => n.0 as u32,
                Some(other) => {
                    panic!("[DD render_button] outline.width must be Number, found {:?}", other);
                }
            };
            let outline = if is_inner {
                Outline::inner().width(width).solid().color(color)
            } else {
                Outline::outer().width(width).solid().color(color)
            };
            Some(outline)
        }
        Some(other) => {
            panic!("[DD render_button] outline must be NoOutline or object, found {:?}", other);
        }
    };

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
        .map(|v| match v {
            Value::Tagged { tag, .. } => tag.as_ref().to_string(),
            Value::Text(s) => s.to_string(),
            other => {
                panic!("[DD render_stripe] direction must be Text or Tag, found {:?}", other);
            }
        })
        .unwrap_or_else(|| {
            panic!("[DD render_stripe] Missing required 'direction' field");
        });

    let gap = fields
        .get("gap")
        .map(|v| match v {
            Value::Number(n) => n.0 as u32,
            other => panic!("[DD render_stripe] gap must be Number, found {:?}", other),
        })
        .unwrap_or_else(|| {
            panic!("[DD render_stripe] Missing required 'gap' field");
        });

    // Extract hovered LinkRef from element.hovered if present
    let hovered_link_id = fields
        .get("element")
        .and_then(|e| e.get("hovered"))
        .and_then(|v| match v {
            Value::LinkRef(id) => Some(id.to_string()),
            _ => None,
        });

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

    // Phase 2.3: Check if items is a CellRef/Collection (list-backed) for reactive rendering
    let items_hold_ref = fields.get("items").and_then(|v| match v {
        Value::CellRef(cell_id) => Some(cell_id.name().to_string()),
        Value::List(handle) => Some(
            handle.cell_id
                .as_deref()
                .map(str::to_string)
                .unwrap_or_else(|| handle.id.to_string())
        ),
        _ => None,
    });

    if items_hold_ref.is_none() {
        panic!("[DD render_stripe] 'items' must be CellRef or Collection");
    }

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
    match direction.as_str() {
        "Row" => {
            let cell_id = items_hold_ref.as_ref().unwrap_or_else(|| {
                panic!("[DD render_stripe] 'items' must be CellRef or Collection");
            });
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
            row.unify()
        }
        "Column" => {
            let cell_id = items_hold_ref.as_ref().unwrap_or_else(|| {
                panic!("[DD render_stripe] 'items' must be CellRef or Collection");
            });
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
            column.unify()
        }
        other => panic!("[DD render_stripe] direction must be Row/Column, found '{}'", other),
    }
}


/// Render a stack (layered elements).
/// Phase 6: Added CellRef support for reactive layers via layers_signal_vec.
fn render_stack(fields: &Arc<std::collections::BTreeMap<Arc<str>, Value>>) -> RawElOrText {
    // Check if layers is a CellRef/Collection for reactive rendering
    if let Some(value) = fields.get("layers") {
        let reactive_cell_id = match value {
            Value::CellRef(cell_id) => Some(cell_id.name().to_string()),
            Value::List(handle) => Some(
                handle.cell_id
                    .as_deref()
                    .map(str::to_string)
                    .unwrap_or_else(|| handle.id.to_string())
            ),
            _ => None,
        };
        if let Some(cell_id_str) = reactive_cell_id {
            // Reactive path - use layers_signal_vec for O(delta) updates
            zoon::println!("[DD render_stack] Reactive layers from '{}'", cell_id_str);
            return Stack::new()
                .layers_signal_vec(
                    list_signal_vec(cell_id_str)
                        .map(|item| render_dd_value(&item))
                )
                .unify();
        }
    }

    panic!("[DD render_stack] 'layers' must be CellRef or Collection");
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
    let placeholder_value = fields.get("placeholder").unwrap_or_else(|| {
        panic!("[DD TextInput] Missing required 'placeholder' field");
    });
    let placeholder = match placeholder_value {
        Value::Unit => None,
        Value::Text(text) => Some(text.as_ref().to_string()),
        Value::Object(obj) => {
            let text_value = obj.get("text").unwrap_or_else(|| {
                panic!("[DD TextInput] placeholder object must contain 'text'");
            });
            match text_value {
                Value::Text(text) => Some(text.as_ref().to_string()),
                Value::CellRef(cell_id) => {
                    panic!("[DD TextInput] placeholder must be static text, found CellRef({})", cell_id);
                }
                other => {
                    panic!("[DD TextInput] placeholder.text must be Text, found {:?}", other);
                }
            }
        }
        other => {
            panic!("[DD TextInput] placeholder must be Text, Unit, or object with text, found {:?}", other);
        }
    };

    let text_field = fields.get("text").unwrap_or_else(|| {
        panic!("[DD TextInput] Missing required 'text' field");
    });
    zoon::println!("[DD TextInput] text field value: {:?}", text_field);

    let text_cell_id = match text_field {
        Value::CellRef(cell_id) => Some(cell_id.to_string()),
        Value::Text(_) => None,
        other => {
            panic!("[DD TextInput] text must be Text or CellRef, found {:?}", other);
        }
    };
    let text = match text_field {
        Value::CellRef(cell_id) => {
            zoon::println!("[DD TextInput] text field is CellRef({})", cell_id);
            String::new()
        }
        Value::Text(text) => text.as_ref().to_string(),
        other => {
            panic!("[DD TextInput] text must be Text or CellRef, found {:?}", other);
        }
    };

    // Check for focus: True tag
    let focus_value = fields.get("focus").unwrap_or_else(|| {
        panic!("[DD TextInput] Missing required 'focus' field");
    });
    let should_focus = match focus_value {
        Value::Tagged { tag, .. } if BoolTag::is_bool_tag(tag.as_ref()) => BoolTag::is_true(tag.as_ref()),
        Value::Bool(b) => *b,
        other => {
            panic!("[DD TextInput] focus must be Bool or BoolTag, found {:?}", other);
        }
    };

    // Extract key_down LinkRef from element.event.key_down
    let key_down_link_id = fields
        .get("element")
        .and_then(|e| e.get("event"))
        .and_then(|e| e.get("key_down"))
        .map(|v| match v {
            Value::LinkRef(id) => id.to_string(),
            other => {
                panic!("[DD TextInput] element.event.key_down must be LinkRef, found {:?}", other);
            }
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
        .map(|v| match v {
            Value::LinkRef(id) => id.to_string(),
            other => {
                panic!("[DD TextInput] element.event.change must be LinkRef, found {:?}", other);
            }
        });

    // Extract blur LinkRef separately (for editing inputs)
    let blur_link_id = fields
        .get("element")
        .and_then(|e| e.get("event"))
        .and_then(|e| e.get("blur"))
        .map(|v| match v {
            Value::LinkRef(id) => id.to_string(),
            other => {
                panic!("[DD TextInput] element.event.blur must be LinkRef, found {:?}", other);
            }
        });

    let is_editing_input = blur_link_id.is_some();

    if text_cell_id.is_some() && change_link_id.is_none() {
        panic!("[DD TextInput] text CellRef requires element.event.change LinkRef");
    }
    if text_cell_id.is_none() && change_link_id.is_some() {
        panic!("[DD TextInput] element.event.change requires text CellRef");
    }

    let build_input = {
        let placeholder_text = placeholder.clone().unwrap_or_default();
        let text = text.clone();
        let text_cell_id = text_cell_id.clone();
        move || {
            let input = if let Some(cell_id) = &text_cell_id {
                let cell_id_for_signal = cell_id.clone();
                TextInput::new()
                    .id("dd_text_input")
                    .text_signal(
                        cell_signal(cell_id.clone())
                            .map(move |value| {
                                let value = value.unwrap_or_else(|| {
                                    panic!("[DD TextInput] Missing cell value for '{}'", cell_id_for_signal);
                                });
                                value.to_display_string()
                            })
                    )
            } else {
                TextInput::new()
                    .id("dd_text_input")
                    .text(text.clone())
            };
            let input = input.placeholder(Placeholder::new(placeholder_text.clone()));

            let do_focus = should_focus;
            let do_double_focus = is_editing_input;
            input
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
                                if do_double_focus {
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
                                } else {
                                    let closure = Closure::once(move || {
                                        let _ = el_clone.focus();
                                    });
                                    if let Some(window) = zoon::web_sys::window() {
                                        let _ = window.request_animation_frame(closure.as_ref().unchecked_ref());
                                    }
                                    closure.forget();
                                }
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
        }
    };

    // TextInput builder uses typestate, so we need separate code paths
    // for different combinations of event handlers
    match (key_down_link_id, change_link_id) {
        (Some(key_link), Some(change_link)) => {
            let input = build_input()
                .on_key_down_event(move |event| {
                    zoon::println!("[DD on_key_down_event] INPUT fired! key_link={}", key_link);
                    match event.key() {
                        Key::Enter => {
                            zoon::println!("[DD on_key_down_event] Enter pressed");
                            #[cfg(target_arch = "wasm32")]
                            {
                                let input_text = get_dd_text_input_value();
                                zoon::println!("[DD on_key_down_event] Enter text captured: '{}'", input_text);
                                fire_global_key_down(&key_link, DdKey::Enter, Some(input_text));
                            }
                            #[cfg(not(target_arch = "wasm32"))]
                            {
                                fire_global_key_down(&key_link, DdKey::Enter, Some(String::new()));
                            }
                            return;
                        }
                        Key::Escape => {
                            fire_global_key_down(&key_link, DdKey::Escape, None);
                        }
                        Key::Other(k) => {
                            fire_global_key_down(&key_link, DdKey::from(k.as_str()), None);
                        }
                    }
                })
                .on_change(move |new_text| {
                    fire_global_link_with_text(&change_link, new_text);
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
            build_input()
                .on_key_down_event(move |event| {
                    zoon::println!("[DD on_key_down_event] SIMPLE INPUT fired! key_link={}", key_link);
                    match event.key() {
                        Key::Enter => {
                            zoon::println!("[DD on_key_down_event] Enter pressed in SIMPLE input");
                            #[cfg(target_arch = "wasm32")]
                            {
                                let input_text = get_dd_text_input_value();
                                fire_global_key_down(&key_link, DdKey::Enter, Some(input_text));
                            }
                            #[cfg(not(target_arch = "wasm32"))]
                            {
                                fire_global_key_down(&key_link, DdKey::Enter, Some(String::new()));
                            }
                            return;
                        }
                        Key::Escape => {
                            fire_global_key_down(&key_link, DdKey::Escape, None);
                        }
                        Key::Other(k) => {
                            fire_global_key_down(&key_link, DdKey::from(k.as_str()), None);
                        }
                    }
                })
                .on_change(|_| {})
                .unify()
        }
        (None, Some(change_link)) => {
            build_input()
                .on_change(move |new_text| {
                    fire_global_link_with_text(&change_link, new_text);
                })
                .unify()
        }
        (None, None) => {
            build_input()
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
    let checked_value = fields
        .get("checked")
        .unwrap_or_else(|| {
            panic!("[DD render_checkbox] Missing required 'checked' field");
        })
        .clone();
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
    if let Value::CellRef(cell_id) = &checked_value {
        // Reactive checkbox - observe HOLD state changes
        let cell_id_for_signal = cell_id.to_string();
        let cell_id_for_checked = cell_id_for_signal.clone();
        let cell_id_for_icon = cell_id.to_string();
        let cell_id_for_icon_checked = cell_id_for_icon.clone();

        // Phase 12: Use granular cell_signal() for checkbox - only updates when this cell changes
        let checkbox = Checkbox::new()
            .label_hidden("checkbox")
            .checked_signal(
                cell_signal(cell_id_for_signal)  // Pass owned String
                    .map(move |value| {
                        let value = value.unwrap_or_else(|| {
                            panic!("[DD Checkbox] Missing cell value for '{}'", cell_id_for_checked);
                        });
                        match value {
                            Value::Bool(b) => b,
                            Value::Tagged { tag, .. } if BoolTag::is_bool_tag(tag.as_ref()) => BoolTag::is_true(tag.as_ref()),
                            other => panic!("[DD Checkbox] Expected Bool for '{}', found {:?}", cell_id_for_checked, other),
                        }
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
                                .map(move |value| {
                                    let value = value.unwrap_or_else(|| {
                                        panic!("[DD Checkbox] Missing cell value for '{}'", cell_id_for_icon_checked);
                                    });
                                    let checked = match value {
                                        Value::Bool(b) => b,
                                        Value::Tagged { tag, .. } if BoolTag::is_bool_tag(tag.as_ref()) => BoolTag::is_true(tag.as_ref()),
                                        other => panic!("[DD Checkbox] Expected Bool for '{}', found {:?}", cell_id_for_icon_checked, other),
                                    };
                                    if checked { CHECKED_SVG } else { UNCHECKED_SVG }
                                })
                        ))
                }
            });

        // For reactive checkboxes with a CellRef, toggle the HOLD value directly
        // Fire the link event - LinkCellMapping::BoolToggle handles the actual toggle
        // NOTE: Don't call toggle_cell_bool directly here! That would cause a double toggle
        // because DD link mappings handle BoolToggle.
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
                        // Only fire link - DD link mappings handle the toggle
                        fire_global_link(&link_id);
                    })
                })
                .unify();
        } else {
            panic!("[DD render_checkbox] Bug: reactive checkbox missing click LinkRef for {}", cell_id_for_toggle);
        }
    }

    // Phase 7: ComputedRef REMOVED - DD computes checkbox state directly
    // The bridge receives final Bool values, not deferred computations

    // Static checkbox - extract checked state directly
    let checked = match checked_value {
        Value::Bool(b) => b,
        Value::Tagged { tag, .. } if BoolTag::is_bool_tag(tag.as_ref()) => BoolTag::is_true(tag.as_ref()),
        other => {
            panic!("[DD render_checkbox] checked must be Bool/BoolTag, found {:?}", other);
        }
    };

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
        .or_else(|| fields.get("text"))
        .unwrap_or_else(|| {
            panic!("[DD render_label] Missing required 'label'/'text' field");
        });

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
        Value::CellRef(name) => {
            // Reactive label - update when HOLD state changes
            // Phase 12: Use granular cell_signal() for label - only updates when this cell changes
            let cell_id = name.to_string();
            let cell_id_for_signal = cell_id.clone();
            Label::new()
                .label_signal(
                    cell_signal(cell_id)  // Pass owned String directly
                        .map(move |value| {
                            let value = value.unwrap_or_else(|| {
                                panic!("[DD Label] Missing cell value for '{}'", cell_id_for_signal);
                            });
                            value.to_display_string()
                        })
                )
                .for_input("dd_text_input")
        }
        // Phase 7: ReactiveText REMOVED - DD produces Text values directly
        v => {
            // Static label
            Label::new()
                .label(v.to_display_string())
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
    let contents_field = fields.get("contents");
    let content_field = fields.get("content");
    let text_field = fields.get("text");
    let field_count = contents_field.is_some() as u8 + content_field.is_some() as u8 + text_field.is_some() as u8;
    if field_count != 1 {
        panic!(
            "[DD render_paragraph] Provide exactly one of 'contents', 'content', or 'text' (found contents={}, content={}, text={})",
            contents_field.is_some(),
            content_field.is_some(),
            text_field.is_some()
        );
    }

    let field_value = contents_field.or(content_field).or(text_field).unwrap_or_else(|| {
        panic!("[DD render_paragraph] Missing required 'contents'/'content'/'text' field");
    });

    if let Value::CellRef(cell_id) = field_value {
        // Reactive path - use content_signal for scalar cells, contents_signal_vec for lists
        let cell_id_str = cell_id.name().to_string();
        if is_list_cell(&cell_id_str) {
            return Paragraph::new()
                .contents_signal_vec(
                    list_signal_vec(cell_id_str)
                        .map(|item| render_dd_value(&item))
                )
                .unify();
        }
        let cell_id_for_signal = cell_id_str.clone();
        zoon::println!("[DD render_paragraph] Reactive content from CellRef '{}'", cell_id_str);
        return Paragraph::new()
            .content_signal(
                cell_signal(cell_id_str)
                    .map(move |value| {
                        let value = value.unwrap_or_else(|| {
                            panic!("[DD Paragraph] Missing cell value for '{}'", cell_id_for_signal);
                        });
                        extract_text_content(&value)
                    })
            )
            .unify();
    }

    match field_value {
        Value::List(handle) => {
            let list_id = handle
                .cell_id
                .as_deref()
                .map(str::to_string)
                .unwrap_or_else(|| handle.id.to_string());
            Paragraph::new()
                .contents_signal_vec(
                    list_signal_vec(list_id)
                        .map(|item| render_dd_value(&item))
                )
                .unify()
        }
        other => {
            let content = extract_text_content(other);
            Paragraph::new()
                .content(content)
                .unify()
        }
    }
}

/// Extract display text from a Value, handling nested elements like links.
fn extract_text_content(value: &Value) -> String {
    match value {
        Value::Text(s) => s.to_string(),
        Value::Unit => " ".to_string(), // Text/space() renders as Unit
        Value::Tagged { tag, fields } if tag.as_ref() == "NoElement" => String::new(),
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
                        .unwrap_or_else(|| {
                            panic!("[DD extract_text_content] link element missing 'label'");
                        })
                }
                Some("paragraph") => {
                    // Recursively extract from paragraph contents
                    if let Some(contents) = fields.get("contents") {
                        match contents {
                            Value::List(_) => {
                                panic!("[DD extract_text_content] paragraph contents cannot be Collection");
                            }
                            _ => {}
                        }
                    }
                    fields.get("content")
                        .or_else(|| fields.get("text"))
                        .map(|v| extract_text_content(v))
                        .unwrap_or_else(|| {
                            panic!("[DD extract_text_content] paragraph element missing 'contents'/'content'/'text'");
                        })
                }
                _ => {
                    // For other elements, try to extract from label or child
                    fields.get("label")
                        .or_else(|| fields.get("child"))
                        .map(|v| extract_text_content(v))
                        .unwrap_or_else(|| {
                            panic!("[DD extract_text_content] element missing 'label' or 'child'");
                        })
                }
            }
        }
        Value::List(_) => {
            panic!("[DD extract_text_content] Collection is not valid text content");
        }
        _ => value.to_display_string(),
    }
}

fn select_while_arm(value: &Value, arms: &Arc<Vec<WhileArm>>, default: &Value) -> Value {
    for arm in arms.iter() {
        if while_pattern_matches(value, &arm.pattern) {
            return arm.body.clone();
        }
    }
    if !matches!(default, Value::Unit) {
        return default.clone();
    }
    Value::Unit
}

fn while_pattern_matches(value: &Value, pattern: &Value) -> bool {
    match (value, pattern) {
        (Value::Bool(b), Value::Tagged { tag, .. }) if BoolTag::is_bool_tag(tag.as_ref()) => {
            BoolTag::matches_bool(tag.as_ref(), *b)
        }
        _ => value == pattern,
    }
}

/// Render a link element.
fn render_link(fields: &Arc<std::collections::BTreeMap<Arc<str>, Value>>) -> RawElOrText {
    let label = fields
        .get("label")
        .map(|v| v.to_display_string())
        .unwrap_or_else(|| {
            panic!("[DD render_link] Missing required 'label' field");
        });

    let to = fields
        .get("to")
        .map(|v| v.to_display_string())
        .unwrap_or_else(|| {
            panic!("[DD render_link] Missing required 'to' field");
        });

    Link::new()
        .label(label)
        .to(to)
        .unify()
}
