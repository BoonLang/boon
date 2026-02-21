//! DOM bridge — creates Zoon UI elements from the IR program and connects
//! them to the WASM runtime instance.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use zoon::*;

use super::ir::*;
use super::runtime::{CellStore, WasmInstance};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Build the Zoon element tree for the given IR program and WASM instance.
/// The tag_table is cloned for reactive text display.
pub fn build_ui(program: &IrProgram, instance: Rc<WasmInstance>) -> RawElOrText {
    let doc_root = match program.document {
        Some(cell) => cell,
        None => return zoon::Text::new("No document root").unify(),
    };

    // Find the Document node to get the root cell.
    let root_cell = find_document_root(program).unwrap_or(doc_root);

    // Build element from the root cell.
    build_cell_element(program, &instance, root_cell)
}

/// Set up the connection between Router/go_to() and Router/route().
/// - Reads the current URL path on init and sets the route cell.
/// - When RouterGoTo source changes, updates route cell + pushes history.
/// - Listens for popstate events to update route cell.
/// Must be called BEFORE call_init() so WHEN/WHILE arms see the correct route text.
pub fn setup_router(program: &IrProgram, instance: &Rc<WasmInstance>) {
    // Find the route cell (named "route") and its event (EventSource::Router).
    let mut route_cell = None;
    let mut route_event = None;
    for (idx, event) in program.events.iter().enumerate() {
        if matches!(event.source, EventSource::Router) {
            route_event = Some(EventId(idx as u32));
            break;
        }
    }
    // Find the route cell by name.
    for (idx, cell_info) in program.cells.iter().enumerate() {
        if cell_info.name == "route" {
            route_cell = Some(CellId(idx as u32));
            break;
        }
    }

    let (route_cell, route_event) = match (route_cell, route_event) {
        (Some(c), Some(e)) => (c, e),
        _ => return, // No Router/route() in this program.
    };

    // Read the actual browser URL path and set the route cell text.
    // This must happen BEFORE call_init() so WHEN/WHILE arms see the correct route.
    // Set f64 to 0.0 so subsequent bumps produce real numbers (not NaN + 1 = NaN).
    let window = web_sys::window().unwrap();
    let path = window.location().pathname().unwrap_or_else(|_| "/".to_string());
    instance.cell_store.set_cell_text(route_cell.0, path.clone());
    instance.cell_store.set_cell_f64(route_cell.0, 0.0);

    // Find all RouterGoTo nodes and set up watchers on their source cells.
    for node in &program.nodes {
        if let IrNode::RouterGoTo { source, .. } = node {
            let source_id = source.0;
            let inst = instance.clone();
            let rc = route_cell;
            let re = route_event;
            // Watch the goto source cell for changes → update route cell + push history.
            let handle = Task::start_droppable(
                inst.cell_store.get_cell_signal(source_id)
                    .for_each_sync(move |_val| {
                        let new_path = inst.cell_store.get_cell_text(source_id);
                        if new_path.is_empty() {
                            return;
                        }
                        // Push to browser history.
                        let window = web_sys::window().unwrap();
                        let history = window.history().unwrap();
                        let _ = history.push_state_with_url(
                            &wasm_bindgen::JsValue::NULL,
                            "",
                            Some(&new_path),
                        );
                        // Update route cell.
                        inst.cell_store.set_cell_text(rc.0, new_path);
                        inst.set_cell_value(rc.0, inst.cell_store.get_cell_value(rc.0) + 1.0);
                        let _ = inst.fire_event(re.0);
                    })
            );
            std::mem::forget(handle);
        }
    }

    // Set up popstate listener for browser back/forward navigation.
    let inst = instance.clone();
    let cb = wasm_bindgen::closure::Closure::wrap(Box::new(move |_event: web_sys::Event| {
        let window = web_sys::window().unwrap();
        let path = window.location().pathname().unwrap_or_else(|_| "/".to_string());
        inst.cell_store.set_cell_text(route_cell.0, path);
        inst.set_cell_value(route_cell.0, inst.cell_store.get_cell_value(route_cell.0) + 1.0);
        let _ = inst.fire_event(route_event.0);
    }) as Box<dyn FnMut(web_sys::Event)>);
    let window = web_sys::window().unwrap();
    let _ = window.add_event_listener_with_callback("popstate", cb.as_ref().unchecked_ref());
    cb.forget();
}

// ---------------------------------------------------------------------------
// Element builders
// ---------------------------------------------------------------------------

fn build_cell_element(program: &IrProgram, instance: &Rc<WasmInstance>, cell: CellId) -> RawElOrText {
    if let Some(node) = find_node_for_cell(program, cell) {
        match node {
            IrNode::Element { kind, links, hovered_cell, .. } => {
                build_element_node(program, instance, kind, links, *hovered_cell)
            }
            IrNode::Derived { expr, .. } => {
                build_expr_element(program, instance, expr, cell)
            }
            IrNode::TextInterpolation { parts, .. } => {
                build_text_interpolation(instance, parts)
            }
            IrNode::PipeThrough { source, .. } => {
                build_cell_element(program, instance, *source)
            }
            IrNode::While { source, arms, .. } => {
                build_while_element(program, instance, cell, *source, arms)
            }
            IrNode::When { source, arms, .. } => {
                build_when_element(program, instance, cell, *source, arms)
            }
            IrNode::Latest { .. }
            | IrNode::MathSum { .. }
            | IrNode::Hold { .. }
            | IrNode::Then { .. }
            | IrNode::ListCount { .. }
            | IrNode::HoldLoop { .. } => {
                build_reactive_text(instance, cell)
            }
            IrNode::ListAppend { source, .. }
            | IrNode::ListClear { source, .. }
            | IrNode::ListRemove { source, .. }
            | IrNode::ListRetain { source, .. }
            | IrNode::RouterGoTo { source, .. } => {
                // These operations pass through to source for rendering.
                build_cell_element(program, instance, *source)
            }
            IrNode::ListIsEmpty { .. }
            | IrNode::TextTrim { .. }
            | IrNode::TextIsNotEmpty { .. } => {
                build_reactive_text(instance, cell)
            }
            IrNode::ListMap { source, item_name, item_cell, template, template_cell_range, template_event_range, .. } => {
                build_list_map(program, instance, cell, *source, *item_cell, item_name, template, *template_cell_range, *template_event_range)
            }
            IrNode::Document { root } => {
                build_cell_element(program, instance, *root)
            }
            IrNode::CustomCall { path, .. } => {
                let path_str = path.join("/");
                zoon::Text::new(format!("[{}]", path_str)).unify()
            }
            _ => zoon::Text::new("?").unify(),
        }
    } else {
        build_reactive_text(instance, cell)
    }
}

/// Build a reactive text element that shows the cell's display value.
fn build_reactive_text(instance: &Rc<WasmInstance>, cell: CellId) -> RawElOrText {
    let store = instance.cell_store.clone();
    let cell_id = cell.0;
    let signal = store.get_cell_signal(cell_id);
    zoon::Text::with_signal(signal.map(move |_| format_cell_value(&store, cell_id))).unify()
}

/// Format a number for display (remove trailing ".0" for integers).
/// Returns empty string for NaN (cell not yet initialized).
fn format_number(n: f64) -> String {
    if n.is_nan() {
        String::new()
    } else if n == n.floor() && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{}", n)
    }
}

/// Format a cell's display value: prefer text content if set, else format f64.
/// Text cells store their content in the text store, with f64 used only as a
/// signal trigger. Number cells have meaningful f64 values.
fn format_cell_value(store: &super::runtime::CellStore, cell_id: u32) -> String {
    let text = store.get_cell_text(cell_id);
    if !text.is_empty() {
        text
    } else {
        format_number(store.get_cell_value(cell_id))
    }
}

/// Build element from an IrExpr (for Derived nodes).
fn build_expr_element(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    expr: &IrExpr,
    cell: CellId,
) -> RawElOrText {
    match expr {
        IrExpr::CellRead(source) => build_cell_element(program, instance, *source),
        IrExpr::Constant(IrValue::Number(n)) => {
            zoon::Text::new(format_number(*n)).unify()
        }
        IrExpr::Constant(IrValue::Text(t)) => {
            zoon::Text::new(t.clone()).unify()
        }
        IrExpr::Constant(IrValue::Tag(t)) if t == "NoElement" => {
            // NoElement renders as nothing — empty hidden span.
            RawHtmlEl::new("span").style("display", "none").into_raw_unchecked()
        }
        IrExpr::Constant(IrValue::Tag(t)) => {
            zoon::Text::new(t.clone()).unify()
        }
        IrExpr::TextConcat(segments) => {
            build_text_from_segments(instance, segments)
        }
        _ => {
            build_reactive_text(instance, cell)
        }
    }
}

/// Build text from TextSegment list.
fn build_text_interpolation(
    instance: &Rc<WasmInstance>,
    parts: &[TextSegment],
) -> RawElOrText {
    build_text_from_segments(instance, parts)
}

fn build_text_from_segments(
    instance: &Rc<WasmInstance>,
    segments: &[TextSegment],
) -> RawElOrText {
    // Check if any segments are reactive (CellRead).
    let has_reactive = segments.iter().any(|seg| matches!(seg, TextSegment::Expr(IrExpr::CellRead(_))));

    if !has_reactive {
        // Pure static text.
        let text: String = segments
            .iter()
            .map(|seg| match seg {
                TextSegment::Literal(t) => t.clone(),
                _ => String::new(),
            })
            .collect();
        return zoon::Text::new(text).unify();
    }

    // Reactive text: collect cell IDs and build a signal that combines them.
    // Strategy: use the first reactive cell's signal and map over all segments,
    // reading current values from the store for other cells.
    // This works because all cells update through the same CellStore.
    let store = instance.cell_store.clone();

    // Collect all referenced cell IDs for combined signal.
    let cell_ids: Vec<u32> = segments
        .iter()
        .filter_map(|seg| {
            if let TextSegment::Expr(IrExpr::CellRead(cell)) = seg {
                Some(cell.0)
            } else {
                None
            }
        })
        .collect();

    // Build a cloneable description of segments for the closure.
    let seg_desc: Vec<SegDesc> = segments
        .iter()
        .map(|seg| match seg {
            TextSegment::Literal(t) => SegDesc::Lit(t.clone()),
            TextSegment::Expr(IrExpr::CellRead(cell)) => SegDesc::Cell(cell.0),
            _ => SegDesc::Lit(String::new()),
        })
        .collect();

    if cell_ids.len() == 1 {
        // Single reactive cell — use its signal directly.
        let signal = store.get_cell_signal(cell_ids[0]);
        zoon::Text::with_signal(signal.map(move |_| {
            seg_desc.iter().map(|s| match s {
                SegDesc::Lit(t) => t.clone(),
                SegDesc::Cell(id) => format_cell_value(&store, *id),
            }).collect::<String>()
        })).unify()
    } else {
        // Multiple reactive cells — combine signals.
        // Use first cell as primary trigger, poll others from store.
        // This is correct because all signals update atomically per event.
        let primary = store.get_cell_signal(cell_ids[0]);
        zoon::Text::with_signal(primary.map(move |_| {
            seg_desc.iter().map(|s| match s {
                SegDesc::Lit(t) => t.clone(),
                SegDesc::Cell(id) => format_cell_value(&store, *id),
            }).collect::<String>()
        })).unify()
    }
}

/// Segment description for cloning into closures.
#[derive(Clone)]
enum SegDesc {
    Lit(String),
    Cell(u32),
}

/// Build an Element node (Button, TextInput, Stripe, etc.).
fn build_element_node(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    kind: &ElementKind,
    links: &[(String, EventId)],
    hovered_cell: Option<CellId>,
) -> RawElOrText {
    let raw: RawElOrText = match kind {
        ElementKind::Button { label, style } => {
            build_button(program, instance, label, style, links)
        }
        ElementKind::Stripe {
            direction,
            items,
            gap,
            style,
            ..
        } => build_stripe(program, instance, direction, *items, gap, style),
        ElementKind::TextInput { placeholder, style, focus, .. } => {
            build_text_input(program, instance, placeholder.as_ref(), style, links, *focus)
        }
        ElementKind::Checkbox { checked, style, icon } => {
            build_checkbox(program, instance, checked.as_ref(), style, links, icon.as_ref())
        }
        ElementKind::Container { child, style } => {
            build_container(program, instance, *child, style)
        }
        ElementKind::Label { label, style } => {
            build_label(program, instance, label, style, links)
        }
        ElementKind::Stack { layers, style } => {
            build_stack(program, instance, *layers, style)
        }
        ElementKind::Link { url, label, style } => {
            build_link(program, instance, label, url, style, links)
        }
        ElementKind::Paragraph { content, style } => {
            build_paragraph(program, instance, content, style)
        }
    };
    // Attach common event handlers (hovered, blur, focus, click, double_click).
    attach_common_events(raw, instance, links, hovered_cell)
}

/// Attach common event handlers (hovered, blur, focus, click, double_click) to an element.
/// If no common events are needed, returns the element unchanged.
/// For hover, wraps the element in a container div to capture mouse events.
fn attach_common_events(
    el: RawElOrText,
    instance: &Rc<WasmInstance>,
    links: &[(String, EventId)],
    hovered_cell: Option<CellId>,
) -> RawElOrText {
    let has_hover = hovered_cell.is_some();
    let blur_event = links.iter().find(|(n, _)| n == "blur").map(|(_, e)| *e);
    let focus_event = links.iter().find(|(n, _)| n == "focus").map(|(_, e)| *e);
    let click_event = links.iter().find(|(n, _)| n == "click").map(|(_, e)| *e);
    let double_click_event = links.iter().find(|(n, _)| n == "double_click").map(|(_, e)| *e);

    if !has_hover && blur_event.is_none() && focus_event.is_none()
        && click_event.is_none() && double_click_event.is_none()
    {
        return el;
    }

    // Wrap in a div to attach event handlers.
    let mut wrapper = RawHtmlEl::new("div")
        .style("display", "contents")
        .child(el);

    // Hover: mouseenter → set cell to True (1.0), mouseleave → False (0.0).
    if let Some(cell) = hovered_cell {
        let inst = instance.clone();
        wrapper = wrapper.event_handler(move |_: events::MouseEnter| {
            inst.set_cell_value(cell.0, 1.0);
        });
        let inst = instance.clone();
        wrapper = wrapper.event_handler(move |_: events::MouseLeave| {
            inst.set_cell_value(cell.0, 0.0);
        });
    }

    // Blur event.
    if let Some(event_id) = blur_event {
        let inst = instance.clone();
        wrapper = wrapper.event_handler(move |_: events::Blur| {
            let _ = inst.fire_event(event_id.0);
        });
    }

    // Focus event.
    if let Some(event_id) = focus_event {
        let inst = instance.clone();
        wrapper = wrapper.event_handler(move |_: events::Focus| {
            let _ = inst.fire_event(event_id.0);
        });
    }

    // Click event.
    if let Some(event_id) = click_event {
        let inst = instance.clone();
        wrapper = wrapper.event_handler(move |_: events::Click| {
            let _ = inst.fire_event(event_id.0);
        });
    }

    // Double-click event.
    if let Some(event_id) = double_click_event {
        let inst = instance.clone();
        wrapper = wrapper.event_handler(move |_: events::DoubleClick| {
            let _ = inst.fire_event(event_id.0);
        });
    }

    wrapper.into_raw_unchecked()
}

/// Resolve a label expression to TextConcat segments (following CellRead chains).
fn resolve_label_segments<'a>(program: &'a IrProgram, label: &'a IrExpr) -> Option<&'a [TextSegment]> {
    match label {
        IrExpr::TextConcat(segs) => Some(segs),
        IrExpr::CellRead(cell) => {
            if let Some(IrNode::Derived { expr: IrExpr::TextConcat(segs), .. }) = find_node_for_cell(program, *cell) {
                Some(segs)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Build a label child element — reactive if it contains CellRead segments, static otherwise.
fn build_label_child(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    label: &IrExpr,
) -> RawElOrText {
    if let Some(segs) = resolve_label_segments(program, label) {
        if segs.iter().any(|s| matches!(s, TextSegment::Expr(IrExpr::CellRead(_)))) {
            return build_text_from_segments(instance, segs);
        }
    }
    zoon::Text::new(resolve_static_text(program, label)).unify()
}

/// Build a Button element.
fn build_button(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    label: &IrExpr,
    style: &IrExpr,
    links: &[(String, EventId)],
) -> RawElOrText {
    let press_event = links
        .iter()
        .find(|(name, _)| name == "press")
        .map(|(_, eid)| *eid);

    let mut raw = RawHtmlEl::new("button")
        .attr("role", "button")
        .style("cursor", "pointer")
        .style("border", "none")
        .style("background", "transparent")
        .style("color", "inherit")
        .child(build_label_child(program, instance, label));

    raw = apply_styles(raw, style, program, instance);

    if let Some(event_id) = press_event {
        let inst = instance.clone();
        raw = raw.event_handler(move |_: events::Click| {
            let _ = inst.fire_event(event_id.0);
        });
    }

    raw.into_raw_unchecked()
}

/// Build a Stripe (row/column) element.
fn build_stripe(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    direction: &IrExpr,
    items_cell: CellId,
    gap: &IrExpr,
    style: &IrExpr,
) -> RawElOrText {
    let children = collect_stripe_children(program, instance, items_cell);

    let is_column = match direction {
        IrExpr::Constant(IrValue::Tag(t)) => t == "Column",
        _ => true,
    };

    let mut raw = RawHtmlEl::new("div");
    if is_column {
        raw = raw.style("display", "inline-flex").style("flex-direction", "column");
    } else {
        raw = raw
            .style("display", "inline-flex")
            .style("flex-direction", "row")
            .style("align-items", "center");
    }

    // Apply gap if specified.
    if let IrExpr::Constant(IrValue::Number(n)) = gap {
        if *n > 0.0 {
            raw = raw.style("gap", &format!("{}px", n));
        }
    }

    raw = apply_styles(raw, style, program, instance);
    raw = raw.children(children);
    raw.into_raw_unchecked()
}

/// Collect children for a stripe element.
fn collect_stripe_children(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    items_cell: CellId,
) -> Vec<RawElOrText> {
    match find_node_for_cell(program, items_cell) {
        Some(IrNode::Derived { expr, .. }) => {
            match expr {
                IrExpr::ListConstruct(items) => {
                    items
                        .iter()
                        .filter_map(|item| {
                            match item {
                                IrExpr::CellRead(child_cell) => {
                                    Some(build_cell_element(program, instance, *child_cell))
                                }
                                IrExpr::Constant(IrValue::Text(t)) => {
                                    Some(zoon::Text::new(t.clone()).unify())
                                }
                                IrExpr::Constant(IrValue::Tag(t)) => {
                                    if t == "NoElement" {
                                        None
                                    } else {
                                        Some(zoon::Text::new(t.clone()).unify())
                                    }
                                }
                                IrExpr::TextConcat(segments) => {
                                    Some(build_text_from_segments(instance, segments))
                                }
                                _ => None,
                            }
                        })
                        .collect()
                }
                IrExpr::CellRead(source) => {
                    collect_stripe_children(program, instance, *source)
                }
                _ => Vec::new(),
            }
        }
        Some(_) => {
            // Non-Derived node (e.g., ListMap) — render it as a single child element.
            vec![build_cell_element(program, instance, items_cell)]
        }
        None => Vec::new(),
    }
}

/// Build a TextInput element with event handlers for key_down and change.
fn build_text_input(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    placeholder: Option<&IrExpr>,
    style: &IrExpr,
    links: &[(String, EventId)],
    focus: bool,
) -> RawElOrText {
    let mut raw = RawHtmlEl::new("input")
        .attr("type", "text")
        .style("box-sizing", "border-box")
        .style("border", "none")
        .style("outline", "none")
        .style("color", "inherit")
        .style("background", "transparent");

    if focus {
        raw = raw.attr("autofocus", "");
        // Also programmatically focus after mount via after_insert.
        raw = raw.after_insert(|el| {
            let _ = el.focus();
        });
    }

    raw = apply_styles(raw, style, program, instance);

    // Extract placeholder text — could be nested in an object with text field.
    let raw = if let Some(ph) = placeholder {
        let text = extract_placeholder_text(ph, program);
        if !text.is_empty() {
            raw.attr("placeholder", &text)
        } else {
            raw
        }
    } else {
        raw
    };

    // Find events from links.
    let key_down_event = links.iter()
        .find(|(name, _)| name == "key_down")
        .map(|(_, eid)| *eid);
    let change_event = links.iter()
        .find(|(name, _)| name == "change")
        .map(|(_, eid)| *eid);

    // Find data cells for event payloads by searching the program's cells.
    // Convention: LINK placeholder cells have associated data cells named
    // "{element_name}.event.key_down.key", "{element_name}.event.change.text", "{element_name}.text".
    // We find them by looking for cells whose names end with these suffixes and match our events.
    let key_data_cell = find_data_cell_for_event(program, links, "key_down", "key");
    let change_text_cell = find_data_cell_for_event(program, links, "change", "text");
    let text_cell = find_text_property_cell(program, links);

    // Set up keydown event listener.
    let raw = if let Some(event_id) = key_down_event {
        let inst = instance.clone();
        raw.event_handler(move |event: events::KeyDown| {
            let key = event.key();
            let tag_value = match key.as_str() {
                "Enter" => inst.program_tag_index("Enter"),
                "Escape" => inst.program_tag_index("Escape"),
                _ => 0.0,
            };
            // Read input text from the DOM element.
            if let Some(target) = event.target() {
                if let Ok(input) = target.dyn_into::<web_sys::HtmlInputElement>() {
                    let input_text = input.value();
                    // Store text in the change text cell and the .text property cell.
                    if let Some(cell_id) = change_text_cell {
                        inst.cell_store.set_cell_text(cell_id, input_text.clone());
                    }
                    if let Some(cell_id) = text_cell {
                        inst.cell_store.set_cell_text(cell_id, input_text.clone());
                    }
                    // Clear the input if Enter was pressed.
                    if key == "Enter" {
                        input.set_value("");
                    }
                }
            }
            // Set the key data cell.
            if let Some(cell_id) = key_data_cell {
                inst.set_cell_value(cell_id, tag_value);
            }
            // Fire the event.
            let _ = inst.fire_event(event_id.0);
        })
    } else {
        raw
    };

    // Set up input change event listener.
    let raw = if let Some(event_id) = change_event {
        let inst = instance.clone();
        raw.event_handler(move |event: events::Input| {
            // Read input text from the DOM element.
            if let Some(target) = event.target() {
                if let Ok(input) = target.dyn_into::<web_sys::HtmlInputElement>() {
                    let input_text = input.value();
                    if let Some(cell_id) = change_text_cell {
                        inst.cell_store.set_cell_text(cell_id, input_text.clone());
                    }
                    if let Some(cell_id) = text_cell {
                        inst.cell_store.set_cell_text(cell_id, input_text);
                    }
                }
            }
            let _ = inst.fire_event(event_id.0);
        })
    } else {
        raw
    };

    raw.into_raw_unchecked()
}

/// Find a data cell for an event payload (e.g., key_down.key, change.text).
/// Searches the program's cells for names matching "{element_path}.event.{event_name}.{field}".
fn find_data_cell_for_event(
    program: &IrProgram,
    links: &[(String, EventId)],
    event_name: &str,
    field_name: &str,
) -> Option<u32> {
    find_data_cell_for_event_in_range(program, links, event_name, field_name, None)
}

/// Find event data cell, optionally restricted to a template cell range.
/// When a range is given, prefer cells within that range (for per-item templates).
/// Falls back to any matching cell if none found in the range.
fn find_data_cell_for_event_in_range(
    program: &IrProgram,
    links: &[(String, EventId)],
    event_name: &str,
    field_name: &str,
    template_range: Option<(u32, u32)>,
) -> Option<u32> {
    // Find the event to get its name (which contains the element path).
    let event_id = links.iter()
        .find(|(name, _)| name == event_name)
        .map(|(_, eid)| *eid)?;
    // The event's name is "{element_path}.{event_name}".
    let event_info = &program.events[event_id.0 as usize];
    // Strip the event name suffix to get the element path.
    let element_path = event_info.name.strip_suffix(&format!(".{}", event_name))?;
    // Look for the data cell.
    let data_cell_name = format!("{}.event.{}.{}", element_path, event_name, field_name);
    // If a template range is given, prefer cells within that range.
    if let Some((start, end)) = template_range {
        let template_match = program.cells.iter().enumerate()
            .find(|(idx, info)| {
                let id = *idx as u32;
                id >= start && id < end && info.name == data_cell_name
            })
            .map(|(idx, _)| idx as u32);
        if template_match.is_some() {
            return template_match;
        }
    }
    program.cells.iter().enumerate()
        .find(|(_, info)| info.name == data_cell_name)
        .map(|(idx, _)| idx as u32)
}

/// Find the .text property cell for a text input element.
fn find_text_property_cell(
    program: &IrProgram,
    links: &[(String, EventId)],
) -> Option<u32> {
    find_text_property_cell_in_range(program, links, None)
}

/// Find text property cell, optionally restricted to a template cell range.
fn find_text_property_cell_in_range(
    program: &IrProgram,
    links: &[(String, EventId)],
    template_range: Option<(u32, u32)>,
) -> Option<u32> {
    // Use any link event to find the element path.
    let event_id = links.first().map(|(_, eid)| *eid)?;
    let event_info = &program.events[event_id.0 as usize];
    // Strip everything after the last '.' to get "{element_path}.{event_name}",
    // then strip event_name to get element_path.
    let dot_pos = event_info.name.rfind('.')?;
    let element_path = &event_info.name[..dot_pos];
    let text_cell_name = format!("{}.text", element_path);
    // If a template range is given, prefer cells within that range.
    if let Some((start, end)) = template_range {
        let template_match = program.cells.iter().enumerate()
            .find(|(idx, info)| {
                let id = *idx as u32;
                id >= start && id < end && info.name == text_cell_name
            })
            .map(|(idx, _)| idx as u32);
        if template_match.is_some() {
            return template_match;
        }
    }
    program.cells.iter().enumerate()
        .find(|(_, info)| info.name == text_cell_name)
        .map(|(idx, _)| idx as u32)
}

/// Build a Container element (wraps a single child).
fn build_container(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    child: CellId,
    style: &IrExpr,
) -> RawElOrText {
    let child_el = build_cell_element(program, instance, child);
    let mut raw = RawHtmlEl::new("div").child(child_el);
    raw = apply_styles(raw, style, program, instance);
    raw.into_raw_unchecked()
}

/// Build a Label element (displays text).
fn build_label(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    label: &IrExpr,
    style: &IrExpr,
    _links: &[(String, EventId)],
) -> RawElOrText {
    let content: RawElOrText = match label {
        IrExpr::TextConcat(segments) => {
            build_text_from_segments(instance, segments)
        }
        IrExpr::Constant(IrValue::Text(t)) => {
            zoon::Text::new(t.clone()).unify()
        }
        IrExpr::CellRead(cell) => {
            // Follow through to the cell's node (e.g., TextInterpolation)
            // instead of showing raw f64 value.
            build_cell_element(program, instance, *cell)
        }
        _ => {
            let text = eval_static_text(label);
            zoon::Text::new(text).unify()
        }
    };
    // Wrap in a span with styles applied.
    let mut raw = RawHtmlEl::new("span").child(content);
    raw = apply_styles(raw, style, program, instance);
    raw.into_raw_unchecked()
}

/// Build a Stack element (z-axis layering).
fn build_stack(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    layers_cell: CellId,
    style: &IrExpr,
) -> RawElOrText {
    let children = collect_stripe_children(program, instance, layers_cell);
    let mut raw = RawHtmlEl::new("div")
        .style("position", "relative");
    raw = apply_styles(raw, style, program, instance);
    raw = raw.children(children);
    raw.into_raw_unchecked()
}

/// Build a Link element.
fn build_link(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    label: &IrExpr,
    url: &IrExpr,
    style: &IrExpr,
    _links: &[(String, EventId)],
) -> RawElOrText {
    let label_text = resolve_static_text(program, label);
    let url_text = resolve_static_text(program, url);
    let mut raw = RawHtmlEl::new("a")
        .attr("href", &url_text)
        .attr("target", "_blank")
        .attr("rel", "noopener noreferrer")
        .style("color", "inherit")
        .style("text-decoration", "none")
        .child(zoon::Text::new(label_text));
    raw = apply_styles(raw, style, program, instance);
    raw.into_raw_unchecked()
}

/// Build a Paragraph element.
fn build_paragraph(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    content: &IrExpr,
    style: &IrExpr,
) -> RawElOrText {
    let mut raw = RawHtmlEl::new("p");
    raw = apply_styles(raw, style, program, instance);

    match content {
        IrExpr::TextConcat(segments) => {
            raw.child(build_text_from_segments(instance, segments))
                .into_raw_unchecked()
        }
        IrExpr::CellRead(cell) => {
            // Check if it's a list of items (for `contents: LIST { ... }`).
            if let Some(node) = find_node_for_cell(program, *cell) {
                match node {
                    IrNode::Derived { expr: IrExpr::ListConstruct(items), .. } => {
                        let children: Vec<RawElOrText> = items.iter().map(|item| {
                            match item {
                                IrExpr::CellRead(child_cell) => {
                                    build_cell_element(program, instance, *child_cell)
                                }
                                IrExpr::Constant(IrValue::Text(t)) => {
                                    zoon::Text::new(t.clone()).unify()
                                }
                                _ => {
                                    let t = eval_static_text(item);
                                    zoon::Text::new(t).unify()
                                }
                            }
                        }).collect();
                        raw.children(children).into_raw_unchecked()
                    }
                    _ => {
                        raw.child(build_cell_element(program, instance, *cell))
                            .into_raw_unchecked()
                    }
                }
            } else {
                raw.child(build_reactive_text(instance, *cell))
                    .into_raw_unchecked()
            }
        }
        IrExpr::ListConstruct(items) => {
            let children: Vec<RawElOrText> = items.iter().map(|item| {
                match item {
                    IrExpr::CellRead(child_cell) => {
                        build_cell_element(program, instance, *child_cell)
                    }
                    IrExpr::Constant(IrValue::Text(t)) => {
                        zoon::Text::new(t.clone()).unify()
                    }
                    _ => {
                        let t = eval_static_text(item);
                        zoon::Text::new(t).unify()
                    }
                }
            }).collect();
            raw.children(children).into_raw_unchecked()
        }
        _ => {
            let text = eval_static_text(content);
            raw.child(zoon::Text::new(text)).into_raw_unchecked()
        }
    }
}

/// Build a Checkbox element.
/// When an `icon` is provided, renders as a `<label>` with a hidden `<input>` and the icon
/// as a custom visual replacement (matching Zoon's Checkbox component behavior).
fn build_checkbox(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    checked: Option<&CellId>,
    style: &IrExpr,
    links: &[(String, EventId)],
    icon: Option<&CellId>,
) -> RawElOrText {
    // NOTE: Click handler is NOT added here — attach_common_events wraps this element
    // with a <div display:contents> that handles click/blur/focus/etc. Adding a click
    // handler directly on the <input> would cause double-firing due to event bubbling.

    // Build the hidden <input type="checkbox">
    // role="checkbox" is placed on the label (when icon present) so click detection
    // tools can find a visible element with that role.
    let mut input = RawHtmlEl::new("input")
        .attr("type", "checkbox")
        .style("position", "absolute")
        .style("opacity", "0")
        .style("width", "0")
        .style("height", "0")
        .style("margin", "0")
        .style("padding", "0");

    // Reactively bind the `checked` property to the cell value.
    if let Some(&checked_cell) = checked {
        let store = instance.cell_store.clone();
        let cell_id = checked_cell.0;
        let initial = store.get_cell_value(cell_id) != 0.0;
        if initial {
            input = input.attr("checked", "");
        }
        input = input.after_insert(move |el: web_sys::HtmlElement| {
            let handle = Task::start_droppable(
                store.get_cell_signal(cell_id)
                    .for_each_sync(move |val| {
                        let is_checked = val != 0.0;
                        if let Some(inp) = el.dyn_ref::<web_sys::HtmlInputElement>() {
                            inp.set_checked(is_checked);
                        }
                    })
            );
            std::mem::forget(handle);
        });
    }

    // If an icon is provided, wrap with the icon as visual representation.
    // Use <div> instead of <label> to avoid native label click-forwarding
    // which interferes with the event wrapper from attach_common_events.
    if let Some(&icon_cell) = icon {
        let icon_el = build_cell_element(program, instance, icon_cell);
        let mut wrapper = RawHtmlEl::new("div")
            .attr("role", "checkbox")
            .style("display", "flex")
            .style("align-items", "center")
            .style("cursor", "pointer")
            .style("position", "relative");
        // Reactively set aria-checked for accessibility and test tooling.
        if let Some(&checked_cell) = checked {
            let store2 = instance.cell_store.clone();
            let cell_id2 = checked_cell.0;
            let initial2 = store2.get_cell_value(cell_id2) != 0.0;
            wrapper = wrapper.attr("aria-checked", if initial2 { "true" } else { "false" });
            wrapper = wrapper.after_insert(move |el: web_sys::HtmlElement| {
                let handle = Task::start_droppable(
                    store2.get_cell_signal(cell_id2)
                        .for_each_sync(move |val| {
                            let _ = el.set_attribute("aria-checked", if val != 0.0 { "true" } else { "false" });
                        })
                );
                std::mem::forget(handle);
            });
        }
        wrapper = apply_styles(wrapper, style, program, instance);
        wrapper = wrapper
            .child(input)
            .child(icon_el);
        wrapper.into_raw_unchecked()
    } else {
        // No icon — visible checkbox input with role for click detection.
        input = input.attr("role", "checkbox");
        input = apply_styles(input, style, program, instance);
        input.into_raw_unchecked()
    }
}

/// Build a WHILE element — reactively switches child based on source cell value.
fn build_while_element(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    cell: CellId,
    source: CellId,
    arms: &[(IrPattern, IrExpr)],
) -> RawElOrText {
    if has_element_arms(program, arms) {
        build_conditional_element(program, instance, source, arms)
    } else {
        build_reactive_text(instance, cell)
    }
}

/// Build a WHEN element — same bridge behavior as WHILE.
fn build_when_element(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    cell: CellId,
    source: CellId,
    arms: &[(IrPattern, IrExpr)],
) -> RawElOrText {
    if has_element_arms(program, arms) {
        build_conditional_element(program, instance, source, arms)
    } else {
        build_reactive_text(instance, cell)
    }
}

/// Check if any arm body produces an element (vs a text/number value).
/// Element-producing arms use build_conditional_element for visibility toggling.
/// Value-producing arms use build_reactive_text to show the WHEN/WHILE target cell.
fn has_element_arms(program: &IrProgram, arms: &[(IrPattern, IrExpr)]) -> bool {
    arms.iter().any(|(_, body)| is_element_body(program, body))
}

fn is_element_body(program: &IrProgram, body: &IrExpr) -> bool {
    match body {
        IrExpr::CellRead(cell) => {
            if let Some(node) = find_node_for_cell(program, *cell) {
                matches!(node,
                    IrNode::Element { .. }
                    | IrNode::When { .. }
                    | IrNode::While { .. }
                    | IrNode::PipeThrough { .. }
                    | IrNode::ListMap { .. }
                )
            } else {
                false
            }
        }
        IrExpr::Constant(IrValue::Tag(t)) if t == "NoElement" => true,
        _ => false,
    }
}

/// Shared implementation for WHEN/WHILE conditional element rendering.
/// Matches the source cell's current value against arm patterns and renders
/// the matching arm's body as an element.
///
/// Strategy: wrap each arm's element in a div, set initial display based on
/// current value, then use after_insert + Task to watch the signal and toggle
/// display via raw DOM API.
/// When a WHILE conditional arm becomes visible, focus any child input
/// with the `autofocus` attribute. This is needed because `after_insert`
/// fires at mount time when the element may be hidden (`display:none`),
/// and calling `.focus()` on a hidden element has no effect.
fn focus_autofocus_child(el: &web_sys::HtmlElement) {
    if let Ok(Some(input)) = el.query_selector("input[autofocus]") {
        if let Ok(html_el) = input.dyn_into::<web_sys::HtmlElement>() {
            let _ = html_el.focus();
        }
    }
}

fn build_conditional_element(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    source: CellId,
    arms: &[(IrPattern, IrExpr)],
) -> RawElOrText {
    let tag_table = &program.tag_table;

    // Build a mapping: (pattern matcher, element) for each arm.
    let arm_elements: Vec<(ArmMatcher, RawElOrText)> = arms
        .iter()
        .map(|(pattern, body)| {
            let matcher = pattern_to_matcher(pattern, tag_table);
            let element = match body {
                IrExpr::CellRead(cell) => {
                    if is_no_element(program, *cell) {
                        RawHtmlEl::new("span")
                            .style("display", "none")
                            .into_raw_unchecked()
                    } else {
                        build_cell_element(program, instance, *cell)
                    }
                }
                IrExpr::Constant(IrValue::Tag(t)) if t == "NoElement" => {
                    RawHtmlEl::new("span")
                        .style("display", "none")
                        .into_raw_unchecked()
                }
                IrExpr::TextConcat(segments) => {
                    build_text_from_segments(instance, segments)
                }
                _ => {
                    let text = eval_static_text(body);
                    if text.is_empty() {
                        build_reactive_text(instance, source)
                    } else {
                        zoon::Text::new(text).unify()
                    }
                }
            };
            (matcher, element)
        })
        .collect();

    // Single arm — return directly, no switching needed.
    if arm_elements.len() == 1 {
        return arm_elements.into_iter().next().unwrap().1;
    }

    let matchers: Vec<ArmMatcher> = arm_elements.iter().map(|(m, _)| m.clone()).collect();
    let elements: Vec<RawElOrText> = arm_elements.into_iter().map(|(_, e)| e).collect();

    // Build wrapper divs. Each gets after_insert to capture the DOM element,
    // then a Task watches the source signal and toggles display via raw DOM.
    let store = instance.cell_store.clone();
    let source_id = source.0;
    let mut result_children: Vec<RawElOrText> = Vec::new();
    for (i, element) in elements.into_iter().enumerate() {
        let matchers_clone = matchers.clone();
        let store_clone = store.clone();
        // Set initial display based on current cell value.
        let initial_val = store.get_cell_value(source_id);
        let initial_text = store.get_cell_text(source_id);
        let initial_matched = find_matching_arm_idx(&matchers, initial_val, &initial_text);
        let initially_visible = initial_matched == Some(i);
        let wrapper = RawHtmlEl::new("div")
            .style("display", if initially_visible { "contents" } else { "none" })
            .child(element)
            .after_insert(move |el: web_sys::HtmlElement| {
                // Re-set display in case value changed between build and mount.
                let val = store_clone.get_cell_value(source_id);
                let cell_text = store_clone.get_cell_text(source_id);
                let matched = find_matching_arm_idx(&matchers_clone, val, &cell_text);
                let _ = el.style().set_property("display", if matched == Some(i) { "contents" } else { "none" });

                // Watch for changes.
                let matchers_inner = matchers_clone.clone();
                let store_inner = store_clone.clone();
                let handle = Task::start_droppable(
                    store_clone.get_cell_signal(source_id)
                        .for_each_sync(move |val| {
                            let cell_text = store_inner.get_cell_text(source_id);
                            let matched = find_matching_arm_idx(&matchers_inner, val, &cell_text);
                            let visible = matched == Some(i);
                            let _ = el.style().set_property("display", if visible { "contents" } else { "none" });
                            // When becoming visible, focus any autofocus input inside.
                            if visible {
                                focus_autofocus_child(&el);
                            }
                        })
                );
                std::mem::forget(handle);
            });
        result_children.push(wrapper.into_raw_unchecked());
    }

    RawHtmlEl::new("div")
        .style("display", "contents")
        .children(result_children)
        .into_raw_unchecked()
}

/// Check if a cell resolves to the NoElement tag.
fn is_no_element(program: &IrProgram, cell: CellId) -> bool {
    if let Some(node) = find_node_for_cell(program, cell) {
        match node {
            IrNode::Derived { expr: IrExpr::Constant(IrValue::Tag(t)), .. } => t == "NoElement",
            IrNode::Derived { expr: IrExpr::CellRead(inner), .. } => is_no_element(program, *inner),
            _ => false,
        }
    } else {
        false
    }
}

/// Pattern matcher for arm selection.
#[derive(Clone, Debug)]
enum ArmMatcher {
    Tag(f64),       // Match encoded tag value
    Number(f64),    // Match exact number
    Bool(f64),      // Match 0.0 or 1.0
    Text(String),   // Match cell text value
    Wildcard,       // Match anything
}

fn pattern_to_matcher(pattern: &IrPattern, tag_table: &[String]) -> ArmMatcher {
    match pattern {
        IrPattern::Tag(name) => {
            // Encode tag name to its f64 index (1-based).
            if let Some(idx) = tag_table.iter().position(|t| t == name) {
                ArmMatcher::Tag((idx + 1) as f64)
            } else if name == "True" {
                ArmMatcher::Bool(1.0)
            } else if name == "False" {
                ArmMatcher::Bool(0.0)
            } else {
                ArmMatcher::Tag(0.0) // Unknown tag — won't match
            }
        }
        IrPattern::Number(n) => ArmMatcher::Number(*n),
        IrPattern::Wildcard | IrPattern::Binding(_) => ArmMatcher::Wildcard,
        IrPattern::Text(s) => ArmMatcher::Text(s.clone()),
    }
}

fn find_matching_arm<T>(arms: &[(ArmMatcher, T)], value: f64, cell_text: &str) -> Option<usize> {
    find_matching_arm_idx(&arms.iter().map(|(m, _)| m.clone()).collect::<Vec<_>>(), value, cell_text)
}

fn find_matching_arm_idx(matchers: &[ArmMatcher], value: f64, cell_text: &str) -> Option<usize> {
    for (i, matcher) in matchers.iter().enumerate() {
        match matcher {
            ArmMatcher::Tag(tag_val) => {
                if value == *tag_val {
                    return Some(i);
                }
            }
            ArmMatcher::Number(n) => {
                if value == *n {
                    return Some(i);
                }
            }
            ArmMatcher::Bool(b) => {
                if value == *b {
                    return Some(i);
                }
            }
            ArmMatcher::Text(s) => {
                if cell_text == s {
                    return Some(i);
                }
            }
            ArmMatcher::Wildcard => {
                return Some(i);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// List rendering — per-item element building
// ---------------------------------------------------------------------------

/// Context for per-item element building.
/// Carries the item index and template cell/event ranges so we can route
/// cell reads and event wiring to per-item storage vs global storage.
#[derive(Clone)]
struct ItemContext {
    item_idx: u32,
    template_cell_range: (u32, u32),
    template_event_range: (u32, u32),
}

impl ItemContext {
    fn is_template_cell(&self, cell: CellId) -> bool {
        cell.0 >= self.template_cell_range.0 && cell.0 < self.template_cell_range.1
    }

    fn is_template_event(&self, event: EventId) -> bool {
        event.0 >= self.template_event_range.0 && event.0 < self.template_event_range.1
    }
}

/// Build a list map element that reactively renders items with per-item elements.
fn build_list_map(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    map_cell: CellId,
    source: CellId,
    item_cell: CellId,
    _item_name: &str,
    template: &IrNode,
    template_cell_range: (u32, u32),
    template_event_range: (u32, u32),
) -> RawElOrText {
    let store = instance.cell_store.clone();


    // Get the version signal from the map cell to trigger re-renders.
    let version_signal = store.get_cell_signal(map_cell.0);

    // Determine the template cell to follow into the per-item element tree.
    let template_root_cell = match template {
        IrNode::Derived { expr: IrExpr::CellRead(cell), .. } => Some(*cell),
        _ => None,
    };

    // Collect cross-scope event IDs (global events that trigger template-scoped nodes).
    // These need to be forwarded to all live items when the global event fires.
    let cross_scope_events = collect_cross_scope_events(program, template_cell_range, template_event_range);

    let inst = instance.clone();

    // Track live item count for cross-scope event forwarding.
    let live_items: Rc<RefCell<u32>> = Rc::new(RefCell::new(0));
    let live_items_for_closure = live_items.clone();

    // Set up cross-scope event forwarding: when a global event fires,
    // forward it to all live items via on_item_event.
    if !cross_scope_events.is_empty() {
        let cross_events = cross_scope_events.clone();
        let live_ref = live_items.clone();
        let inst_hook = inst.clone();
        inst.add_post_event_hook(Box::new(move |event_id| {
            if cross_events.contains(&event_id) {
                let count = *live_ref.borrow();
                for i in 0..count {
                    // Use batch version — fire_event calls rerun_retain_filters once at end.
                    let _ = inst_hook.call_on_item_event_batch(i, event_id);
                }
            }
        }));
    }

    // Track the number of items that have been initialized.
    // On re-renders (e.g. filter changes), existing items keep their HOLD state;
    // only newly appended items get init_item called.
    let initialized_count: Rc<std::cell::Cell<usize>> = Rc::new(std::cell::Cell::new(0));

    // Deduplicate the version signal: rerun_retain_filters bumps the version
    // even when the filter result hasn't changed (e.g. on change events that
    // don't affect filtering). Without deduplication, every keystroke in an
    // edit input destroys and recreates all list elements, losing the user's
    // in-progress edits.
    //
    // We track previous item indices (memory indices for each list position)
    // and only fire when they actually change. This converts the raw version
    // signal into a stable "list content changed" signal.
    let inst_dedup = instance.clone();
    let prev_indices: Rc<RefCell<Vec<u32>>> = Rc::new(RefCell::new(Vec::new()));
    let deduped_signal = version_signal.filter_map(move |_version| {
        let current_list_id = inst_dedup.cell_store.get_cell_value(source.0);
        let text_items = inst_dedup.list_store.items_text(current_list_id);
        let f64_items = inst_dedup.list_store.items(current_list_id);
        let item_count = if !text_items.is_empty() { text_items.len() } else { f64_items.len() };
        let current: Vec<u32> = (0..item_count)
            .map(|i| inst_dedup.list_store.item_memory_index(current_list_id, i) as u32)
            .collect();
        let mut prev = prev_indices.borrow_mut();
        if *prev == current {
            None // Same items — suppress signal, don't re-render
        } else {
            *prev = current;
            Some(()) // Items changed — allow signal through
        }
    });

    // Build a container that reactively re-renders children when the list changes.
    RawHtmlEl::new("div")
        .style("display", "contents")
        .child_signal(deduped_signal.map(move |_opt| {
            // Re-read list_id from source cell each time — the filter loop may
            // have replaced the list with a new filtered copy.
            let current_list_id = inst.cell_store.get_cell_value(source.0);
            let text_items = inst.list_store.items_text(current_list_id);
            let f64_items = inst.list_store.items(current_list_id);
            let item_count = if !text_items.is_empty() { text_items.len() } else { f64_items.len() };
            if item_count == 0 {
                *live_items_for_closure.borrow_mut() = 0;
                // Reset initialized count so all items re-initialize on next render.
                initialized_count.set(0);
                return None;
            }

            *live_items_for_closure.borrow_mut() = item_count as u32;

            let program = &inst.program;
            let prev_init = initialized_count.get();
            // If the list shrank (items were removed), reset init tracking
            // so re-added items at reused positions get init_item called.
            let prev_init = if item_count < prev_init { 0 } else { prev_init };

            let children: Vec<RawElOrText> = (0..item_count).map(|i| {
                // Use item_memory_index to correctly map position → memory index.
                // For index-based lists (after ListRetain/ListRemove), f64 values
                // are original memory indices. For regular lists, use sequential index.
                let item_idx = inst.list_store.item_memory_index(current_list_id, i) as u32;

                // Set per-item text on item_cell from ListStore item data.
                if let Some(ref ics) = inst.item_cell_store {
                    ics.ensure_item(item_idx);
                    if i < text_items.len() {
                        ics.set_text(item_idx, item_cell.0, text_items[i].clone());
                    } else if i < f64_items.len() {
                        // Numeric list items: format value as text for label display.
                        ics.set_text(item_idx, item_cell.0, format_number(f64_items[i]));
                    }
                }

                // Initialize per-item template cells in WASM memory.
                // Only init NEW items — existing items keep their HOLD state
                // (e.g. completed toggle) across re-renders.
                if i >= prev_init {
                    let _ = inst.call_init_item(item_idx);
                }

                // Copy item text to template-local namespace field cells.
                if let Some(ref ics) = inst.item_cell_store {
                    if let Some(fields) = program.cell_field_cells.get(&item_cell) {
                        let item_text = ics.get_text(item_idx, item_cell.0);
                        if !item_text.is_empty() {
                            for (name, field_cell) in fields {
                                let in_range = field_cell.0 >= template_cell_range.0
                                    && field_cell.0 < template_cell_range.1;
                                let is_namespace = program.cell_field_cells.contains_key(field_cell);
                                if in_range && !is_namespace {
                                    ics.set_text(item_idx, field_cell.0, item_text.clone());
                                }
                            }
                        }
                    }
                }

                // Build per-item element tree.
                let ctx = ItemContext {
                    item_idx,
                    template_cell_range,
                    template_event_range,
                };

                if let Some(root_cell) = template_root_cell {
                    build_item_element(program, &inst, &ctx, root_cell)
                } else {
                    // Fallback: render item text directly.
                    if i < text_items.len() {
                        zoon::Text::new(text_items[i].clone()).unify()
                    } else if i < f64_items.len() {
                        zoon::Text::new(format_number(f64_items[i])).unify()
                    } else {
                        zoon::Text::new("?").unify()
                    }
                }
            }).collect();

            // Update initialized count so future re-renders don't re-init existing items.
            initialized_count.set(item_count);

            Some(
                RawHtmlEl::new("div")
                    .style("display", "contents")
                    .children(children)
                    .into_raw_unchecked(),
            )
        }))
        .into_raw_unchecked()
}

/// Collect global event IDs that trigger template-scoped nodes (cross-scope events).
fn collect_cross_scope_events(
    program: &IrProgram,
    template_cell_range: (u32, u32),
    template_event_range: (u32, u32),
) -> Vec<u32> {
    let mut result = Vec::new();
    let (cell_start, cell_end) = template_cell_range;
    let (event_start, event_end) = template_event_range;

    for node in &program.nodes {
        let triggers: Vec<u32> = match node {
            IrNode::Hold { cell, trigger_bodies, .. }
                if cell.0 >= cell_start && cell.0 < cell_end =>
            {
                trigger_bodies.iter().map(|(t, _)| t.0).collect()
            }
            IrNode::Then { cell, trigger, .. }
                if cell.0 >= cell_start && cell.0 < cell_end =>
            {
                vec![trigger.0]
            }
            IrNode::Latest { target, arms }
                if target.0 >= cell_start && target.0 < cell_end =>
            {
                arms.iter().filter_map(|arm| arm.trigger.map(|t| t.0)).collect()
            }
            _ => continue,
        };

        for t in triggers {
            // It's cross-scope if the event is OUTSIDE the template range.
            if t < event_start || t >= event_end {
                if !result.contains(&t) {
                    result.push(t);
                }
            }
        }
    }
    result
}

/// Build an element for a per-item cell.
/// For template-scoped cells, reads signals from ItemCellStore.
/// For global cells, reads from the global CellStore (same as build_cell_element).
fn build_item_element(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    ctx: &ItemContext,
    cell: CellId,
) -> RawElOrText {
    if let Some(node) = find_node_for_cell(program, cell) {
        match node {
            IrNode::Element { kind, links, hovered_cell, .. } => {
                build_item_element_node(program, instance, ctx, kind, links, *hovered_cell)
            }
            IrNode::Derived { expr, .. } => {
                match expr {
                    IrExpr::CellRead(source) => build_item_element(program, instance, ctx, *source),
                    IrExpr::Constant(IrValue::Text(t)) => zoon::Text::new(t.clone()).unify(),
                    IrExpr::Constant(IrValue::Tag(t)) if t == "NoElement" => {
                        RawHtmlEl::new("span").style("display", "none").into_raw_unchecked()
                    }
                    IrExpr::Constant(IrValue::Tag(t)) => zoon::Text::new(t.clone()).unify(),
                    IrExpr::Constant(IrValue::Number(n)) => zoon::Text::new(format_number(*n)).unify(),
                    IrExpr::TextConcat(segments) => {
                        build_item_text_from_segments(instance, ctx, segments)
                    }
                    _ => {
                        if ctx.is_template_cell(cell) {
                            build_item_reactive_text(instance, ctx, cell)
                        } else {
                            build_reactive_text(instance, cell)
                        }
                    }
                }
            }
            IrNode::TextInterpolation { parts, .. } => {
                build_item_text_from_segments(instance, ctx, parts)
            }
            IrNode::PipeThrough { source, .. } => {
                build_item_element(program, instance, ctx, *source)
            }
            IrNode::While { source, arms, .. } => {
                if has_element_arms(program, arms) {
                    build_item_conditional_element(program, instance, ctx, *source, arms)
                } else if ctx.is_template_cell(cell) {
                    build_item_reactive_text(instance, ctx, cell)
                } else {
                    build_reactive_text(instance, cell)
                }
            }
            IrNode::When { source, arms, .. } => {
                if has_element_arms(program, arms) {
                    build_item_conditional_element(program, instance, ctx, *source, arms)
                } else if ctx.is_template_cell(cell) {
                    build_item_reactive_text(instance, ctx, cell)
                } else {
                    build_reactive_text(instance, cell)
                }
            }
            IrNode::Latest { .. }
            | IrNode::MathSum { .. }
            | IrNode::Hold { .. }
            | IrNode::Then { .. }
            | IrNode::ListCount { .. }
            | IrNode::HoldLoop { .. }
            | IrNode::ListIsEmpty { .. }
            | IrNode::TextTrim { .. }
            | IrNode::TextIsNotEmpty { .. } => {
                if ctx.is_template_cell(cell) {
                    build_item_reactive_text(instance, ctx, cell)
                } else {
                    build_reactive_text(instance, cell)
                }
            }
            IrNode::ListAppend { source, .. }
            | IrNode::ListClear { source, .. }
            | IrNode::ListRemove { source, .. }
            | IrNode::ListRetain { source, .. }
            | IrNode::RouterGoTo { source, .. } => {
                build_item_element(program, instance, ctx, *source)
            }
            IrNode::ListMap { source, item_name, item_cell, template, template_cell_range, template_event_range, .. } => {
                // Nested list maps — use normal build_list_map.
                build_list_map(program, instance, cell, *source, *item_cell, item_name, template, *template_cell_range, *template_event_range)
            }
            IrNode::Document { root } => {
                build_item_element(program, instance, ctx, *root)
            }
            _ => zoon::Text::new("?").unify(),
        }
    } else {
        // No node found — check if it's a per-item cell.
        if ctx.is_template_cell(cell) {
            build_item_reactive_text(instance, ctx, cell)
        } else {
            build_reactive_text(instance, cell)
        }
    }
}

/// Build reactive text that reads from per-item cell store.
fn build_item_reactive_text(
    instance: &Rc<WasmInstance>,
    ctx: &ItemContext,
    cell: CellId,
) -> RawElOrText {
    let ics = match &instance.item_cell_store {
        Some(ics) => ics.clone(),
        None => return build_reactive_text(instance, cell),
    };
    let item_idx = ctx.item_idx;
    let cell_id = cell.0;
    let signal = ics.get_signal(item_idx, cell_id);
    zoon::Text::with_signal(signal.map(move |_sig_val| {
        let text = ics.get_text(item_idx, cell_id);
        if !text.is_empty() {
            text
        } else {
            let val = ics.get_value(item_idx, cell_id);
            format_number(val)
        }
    })).unify()
}

/// Build text from segments with per-item cell support.
fn build_item_text_from_segments(
    instance: &Rc<WasmInstance>,
    ctx: &ItemContext,
    segments: &[TextSegment],
) -> RawElOrText {
    // Check if any segments reference template-scoped cells.
    let has_item_cells = segments.iter().any(|seg| {
        if let TextSegment::Expr(IrExpr::CellRead(cell)) = seg {
            ctx.is_template_cell(*cell)
        } else {
            false
        }
    });

    if !has_item_cells {
        // No per-item cells — delegate to the global version.
        return build_text_from_segments(instance, segments);
    }

    let ics = match &instance.item_cell_store {
        Some(ics) => ics.clone(),
        None => return build_text_from_segments(instance, segments),
    };

    let store = instance.cell_store.clone();
    let item_idx = ctx.item_idx;

    // Build segment descriptions for the closure.
    let seg_desc: Vec<ItemSegDesc> = segments.iter().map(|seg| match seg {
        TextSegment::Literal(t) => ItemSegDesc::Lit(t.clone()),
        TextSegment::Expr(IrExpr::CellRead(cell)) => {
            if ctx.is_template_cell(*cell) {
                ItemSegDesc::ItemCell(cell.0)
            } else {
                ItemSegDesc::GlobalCell(cell.0)
            }
        }
        _ => ItemSegDesc::Lit(String::new()),
    }).collect();

    // Find a cell to use as signal trigger. Prefer item cells.
    let trigger_cell = segments.iter().find_map(|seg| {
        if let TextSegment::Expr(IrExpr::CellRead(cell)) = seg {
            Some(*cell)
        } else {
            None
        }
    });

    let trigger_cell = match trigger_cell {
        Some(c) => c,
        None => return zoon::Text::new("").unify(),
    };

    if ctx.is_template_cell(trigger_cell) {
        let signal = ics.get_signal(item_idx, trigger_cell.0);
        zoon::Text::with_signal(signal.map(move |_| {
            seg_desc.iter().map(|s| match s {
                ItemSegDesc::Lit(t) => t.clone(),
                ItemSegDesc::ItemCell(id) => {
                    let text = ics.get_text(item_idx, *id);
                    if !text.is_empty() { text } else { format_number(ics.get_value(item_idx, *id)) }
                }
                ItemSegDesc::GlobalCell(id) => format_cell_value(&store, *id),
            }).collect::<String>()
        })).unify()
    } else {
        let signal = store.get_cell_signal(trigger_cell.0);
        zoon::Text::with_signal(signal.map(move |_| {
            seg_desc.iter().map(|s| match s {
                ItemSegDesc::Lit(t) => t.clone(),
                ItemSegDesc::ItemCell(id) => {
                    let text = ics.get_text(item_idx, *id);
                    if !text.is_empty() { text } else { format_number(ics.get_value(item_idx, *id)) }
                }
                ItemSegDesc::GlobalCell(id) => format_cell_value(&store, *id),
            }).collect::<String>()
        })).unify()
    }
}

/// Segment description for per-item text rendering.
#[derive(Clone)]
enum ItemSegDesc {
    Lit(String),
    ItemCell(u32),
    GlobalCell(u32),
}

/// Build an Element node with per-item event wiring.
fn build_item_element_node(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    ctx: &ItemContext,
    kind: &ElementKind,
    links: &[(String, EventId)],
    hovered_cell: Option<CellId>,
) -> RawElOrText {
    let raw: RawElOrText = match kind {
        ElementKind::Button { label, style } => {
            build_item_button(program, instance, ctx, label, style, links)
        }
        ElementKind::Stripe { direction, items, gap, style, .. } => {
            build_item_stripe(program, instance, ctx, direction, *items, gap, style)
        }
        ElementKind::TextInput { placeholder, style, focus, text_cell: reactive_text_cell } => {
            build_item_text_input(program, instance, ctx, placeholder.as_ref(), style, links, *focus, *reactive_text_cell)
        }
        ElementKind::Checkbox { checked, style, icon } => {
            build_item_checkbox(program, instance, ctx, checked.as_ref(), style, links, icon.as_ref())
        }
        ElementKind::Container { child, style } => {
            build_item_container(program, instance, ctx, *child, style)
        }
        ElementKind::Label { label, style } => {
            build_item_label(program, instance, ctx, label, style, links)
        }
        ElementKind::Stack { layers, style } => {
            build_item_stack(program, instance, ctx, *layers, style)
        }
        ElementKind::Link { url, label, style } => {
            build_link(program, instance, label, url, style, links)
        }
        ElementKind::Paragraph { content, style } => {
            build_paragraph(program, instance, content, style)
        }
    };
    // Attach event handlers with per-item routing.
    attach_item_events(raw, instance, ctx, links, hovered_cell)
}

/// Attach event handlers to a per-item element.
/// Template-scoped events route through on_item_event; global events through on_event.
fn attach_item_events(
    el: RawElOrText,
    instance: &Rc<WasmInstance>,
    ctx: &ItemContext,
    links: &[(String, EventId)],
    hovered_cell: Option<CellId>,
) -> RawElOrText {
    let has_hover = hovered_cell.is_some();
    let blur_event = links.iter().find(|(n, _)| n == "blur").map(|(_, e)| *e);
    let focus_event = links.iter().find(|(n, _)| n == "focus").map(|(_, e)| *e);
    let click_event = links.iter().find(|(n, _)| n == "click").map(|(_, e)| *e);
    let double_click_event = links.iter().find(|(n, _)| n == "double_click").map(|(_, e)| *e);

    if !has_hover && blur_event.is_none() && focus_event.is_none()
        && click_event.is_none() && double_click_event.is_none()
    {
        return el;
    }

    let mut wrapper = RawHtmlEl::new("div")
        .style("display", "contents")
        .child(el);

    // Hover: use per-item cell if in template range.
    if let Some(cell) = hovered_cell {
        if ctx.is_template_cell(cell) {
            let ics = instance.item_cell_store.clone();
            let item_idx = ctx.item_idx;
            wrapper = wrapper.event_handler(move |_: events::MouseEnter| {
                if let Some(ref ics) = ics {
                    ics.set_cell(item_idx, cell.0, 1.0);
                }
            });
            let ics = instance.item_cell_store.clone();
            wrapper = wrapper.event_handler(move |_: events::MouseLeave| {
                if let Some(ref ics) = ics {
                    ics.set_cell(item_idx, cell.0, 0.0);
                }
            });
        } else {
            let inst = instance.clone();
            let cell_id = cell.0;
            wrapper = wrapper.event_handler(move |_: events::MouseEnter| {
                inst.set_cell_value(cell_id, 1.0);
            });
            let inst = instance.clone();
            let cell_id = cell.0;
            wrapper = wrapper.event_handler(move |_: events::MouseLeave| {
                inst.set_cell_value(cell_id, 0.0);
            });
        }
    }

    // Event helpers: route to on_item_event or on_event based on scope.
    let item_idx = ctx.item_idx;

    if let Some(event_id) = blur_event {
        let inst = instance.clone();
        let is_template = ctx.is_template_event(event_id);
        wrapper = wrapper.event_handler(move |_: events::Blur| {
            if is_template {
                let _ = inst.call_on_item_event(item_idx, event_id.0);
            } else {
                let _ = inst.fire_event(event_id.0);
            }
        });
    }

    if let Some(event_id) = focus_event {
        let inst = instance.clone();
        let is_template = ctx.is_template_event(event_id);
        wrapper = wrapper.event_handler(move |_: events::Focus| {
            if is_template {
                let _ = inst.call_on_item_event(item_idx, event_id.0);
            } else {
                let _ = inst.fire_event(event_id.0);
            }
        });
    }

    if let Some(event_id) = click_event {
        let inst = instance.clone();
        let is_template = ctx.is_template_event(event_id);
        wrapper = wrapper.event_handler(move |_: events::Click| {
            if is_template {
                let _ = inst.call_on_item_event(item_idx, event_id.0);
            } else {
                let _ = inst.fire_event(event_id.0);
            }
        });
    }

    if let Some(event_id) = double_click_event {
        let inst = instance.clone();
        let is_template = ctx.is_template_event(event_id);
        wrapper = wrapper.event_handler(move |_: events::DoubleClick| {
            if is_template {
                let _ = inst.call_on_item_event(item_idx, event_id.0);
            } else {
                let _ = inst.fire_event(event_id.0);
            }
        });
    }

    wrapper.into_raw_unchecked()
}

/// Build a per-item Button element.
fn build_item_button(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    ctx: &ItemContext,
    label: &IrExpr,
    style: &IrExpr,
    links: &[(String, EventId)],
) -> RawElOrText {
    let label_text = resolve_static_text(program, label);

    let press_event = links.iter()
        .find(|(name, _)| name == "press")
        .map(|(_, eid)| *eid);

    let mut raw = RawHtmlEl::new("button")
        .attr("role", "button")
        .style("cursor", "pointer")
        .style("border", "none")
        .style("background", "transparent")
        .style("color", "inherit")
        .child(zoon::Text::new(label_text));

    raw = apply_styles_item(raw, style, program, instance, ctx);

    if let Some(event_id) = press_event {
        let inst = instance.clone();
        let item_idx = ctx.item_idx;
        let is_template = ctx.is_template_event(event_id);
        raw = raw.event_handler(move |_: events::Click| {
            if is_template {
                let _ = inst.call_on_item_event(item_idx, event_id.0);
            } else {
                let _ = inst.fire_event(event_id.0);
            }
        });
    }

    raw.into_raw_unchecked()
}

/// Build a per-item Stripe element.
fn build_item_stripe(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    ctx: &ItemContext,
    direction: &IrExpr,
    items_cell: CellId,
    gap: &IrExpr,
    style: &IrExpr,
) -> RawElOrText {
    let children = collect_item_stripe_children(program, instance, ctx, items_cell);

    let is_column = match direction {
        IrExpr::Constant(IrValue::Tag(t)) => t == "Column",
        _ => true,
    };

    let mut raw = RawHtmlEl::new("div");
    if is_column {
        raw = raw.style("display", "inline-flex").style("flex-direction", "column");
    } else {
        raw = raw
            .style("display", "inline-flex")
            .style("flex-direction", "row")
            .style("align-items", "center");
    }

    // Apply gap if specified.
    if let IrExpr::Constant(IrValue::Number(n)) = gap {
        if *n > 0.0 {
            raw = raw.style("gap", &format!("{}px", n));
        }
    }

    raw = apply_styles_item(raw, style, program, instance, ctx);
    raw = raw.children(children);
    raw.into_raw_unchecked()
}

/// Collect children for a per-item stripe element.
fn collect_item_stripe_children(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    ctx: &ItemContext,
    items_cell: CellId,
) -> Vec<RawElOrText> {
    match find_node_for_cell(program, items_cell) {
        Some(IrNode::Derived { expr, .. }) => {
            match expr {
                IrExpr::ListConstruct(items) => {
                    items.iter().filter_map(|item| {
                        match item {
                            IrExpr::CellRead(child_cell) => {
                                Some(build_item_element(program, instance, ctx, *child_cell))
                            }
                            IrExpr::Constant(IrValue::Text(t)) => {
                                Some(zoon::Text::new(t.clone()).unify())
                            }
                            IrExpr::Constant(IrValue::Tag(t)) => {
                                if t == "NoElement" { None }
                                else { Some(zoon::Text::new(t.clone()).unify()) }
                            }
                            IrExpr::TextConcat(segments) => {
                                Some(build_item_text_from_segments(instance, ctx, segments))
                            }
                            _ => None,
                        }
                    }).collect()
                }
                IrExpr::CellRead(source) => {
                    collect_item_stripe_children(program, instance, ctx, *source)
                }
                _ => Vec::new(),
            }
        }
        Some(_) => {
            vec![build_item_element(program, instance, ctx, items_cell)]
        }
        None => Vec::new(),
    }
}

/// Build a per-item TextInput element.
fn build_item_text_input(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    ctx: &ItemContext,
    placeholder: Option<&IrExpr>,
    style: &IrExpr,
    links: &[(String, EventId)],
    focus: bool,
    reactive_text_cell: Option<CellId>,
) -> RawElOrText {
    let mut raw = RawHtmlEl::new("input")
        .attr("type", "text")
        .style("box-sizing", "border-box")
        .style("border", "none")
        .style("outline", "none")
        .style("color", "inherit")
        .style("background", "transparent");

    raw = apply_styles_item(raw, style, program, instance, ctx);

    if let Some(ph) = placeholder {
        let text = extract_placeholder_text(ph, program);
        if !text.is_empty() { raw = raw.attr("placeholder", &text); }
    }

    let key_down_event = links.iter().find(|(name, _)| name == "key_down").map(|(_, eid)| *eid);
    let change_event = links.iter().find(|(name, _)| name == "change").map(|(_, eid)| *eid);
    let tmpl_range = Some(ctx.template_cell_range);
    let key_data_cell = find_data_cell_for_event_in_range(program, links, "key_down", "key", tmpl_range);
    let change_text_cell = find_data_cell_for_event_in_range(program, links, "change", "text", tmpl_range);
    let text_cell = find_text_property_cell_in_range(program, links, tmpl_range);

    let item_idx = ctx.item_idx;
    let ics_clone = instance.item_cell_store.clone();

    // Set initial input value from the reactive text cell (e.g., LATEST target)
    // or the .text property cell. For edit inputs, this pre-fills the input
    // with the current title.
    let initial_text_for_insert = {
        // Try reactive_text_cell first (LATEST target with actual text), then .text property cell.
        let cells_to_try: Vec<u32> = reactive_text_cell.iter().map(|c| c.0)
            .chain(text_cell.iter().copied())
            .collect();
        let mut found_text: Option<String> = None;
        for cell_id in cells_to_try {
            let text = if cell_id >= ctx.template_cell_range.0 && cell_id < ctx.template_cell_range.1 {
                instance.item_cell_store.as_ref()
                    .map(|ics| ics.get_text(item_idx, cell_id))
                    .unwrap_or_default()
            } else {
                instance.cell_store.get_cell_text(cell_id)
            };
            if !text.is_empty() {
                found_text = Some(text);
                break;
            }
        }
        found_text
    };
    if focus || initial_text_for_insert.is_some() {
        if focus {
            raw = raw.attr("autofocus", "");
        }
        raw = raw.after_insert(move |el| {
            if initial_text_for_insert.is_some() || focus {
                if let Ok(input) = el.dyn_into::<web_sys::HtmlInputElement>() {
                    if let Some(ref text) = initial_text_for_insert {
                        input.set_value(text);
                    }
                    if focus {
                        let _ = input.focus();
                    }
                }
            }
        });
    }

    let raw = if let Some(event_id) = key_down_event {
        let inst = instance.clone();
        let is_template = ctx.is_template_event(event_id);
        let ics = ics_clone.clone();
        let template_cell_range = ctx.template_cell_range;
        raw.event_handler(move |event: events::KeyDown| {
            let key = event.key();
            let tag_value = match key.as_str() {
                "Enter" => inst.program_tag_index("Enter"),
                "Escape" => inst.program_tag_index("Escape"),
                _ => 0.0,
            };
            if let Some(target) = event.target() {
                if let Ok(input) = target.dyn_into::<web_sys::HtmlInputElement>() {
                    let input_text = input.value();
                    // Store text in data cells (per-item or global).
                    for cell_id_opt in [change_text_cell, text_cell] {
                        if let Some(cell_id) = cell_id_opt {
                            if cell_id >= template_cell_range.0 && cell_id < template_cell_range.1 {
                                if let Some(ref ics) = ics {
                                    ics.set_text(item_idx, cell_id, input_text.clone());
                                }
                            } else {
                                inst.cell_store.set_cell_text(cell_id, input_text.clone());
                            }
                        }
                    }
                }
            }
            // Set key data cell: for template-scoped cells, write to per-item
            // WASM linear memory (where on_item_event reads from), not just
            // the global. Without this, WHEN/HOLD bodies that read the key cell
            // see stale values and don't trigger.
            if let Some(cell_id) = key_data_cell {
                if cell_id >= template_cell_range.0 && cell_id < template_cell_range.1 {
                    inst.set_item_cell_value(item_idx, cell_id, tag_value);
                } else {
                    inst.set_cell_value(cell_id, tag_value);
                }
            }
            if is_template {
                let _ = inst.call_on_item_event(item_idx, event_id.0);
            } else {
                let _ = inst.fire_event(event_id.0);
            }
        })
    } else { raw };

    let raw = if let Some(event_id) = change_event {
        let inst = instance.clone();
        let is_template = ctx.is_template_event(event_id);
        let ics = ics_clone.clone();
        let template_cell_range = ctx.template_cell_range;
        raw.event_handler(move |event: events::Input| {
            if let Some(target) = event.target() {
                if let Ok(input) = target.dyn_into::<web_sys::HtmlInputElement>() {
                    let input_text = input.value();
                    for cell_id_opt in [change_text_cell, text_cell] {
                        if let Some(cell_id) = cell_id_opt {
                            if cell_id >= template_cell_range.0 && cell_id < template_cell_range.1 {
                                if let Some(ref ics) = ics {
                                    ics.set_text(item_idx, cell_id, input_text.clone());
                                }
                            } else {
                                inst.cell_store.set_cell_text(cell_id, input_text.clone());
                            }
                        }
                    }
                }
            }
            if is_template {
                let _ = inst.call_on_item_event(item_idx, event_id.0);
            } else {
                let _ = inst.fire_event(event_id.0);
            }
        })
    } else { raw };

    raw.into_raw_unchecked()
}

/// Build a per-item Checkbox element.
fn build_item_checkbox(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    ctx: &ItemContext,
    checked: Option<&CellId>,
    style: &IrExpr,
    links: &[(String, EventId)],
    icon: Option<&CellId>,
) -> RawElOrText {
    // NOTE: Click handler is NOT added here — attach_item_events wraps this element
    // with a <div display:contents> that handles click/blur/focus/etc. Adding a click
    // handler directly on the <input> would cause double-firing due to event bubbling.
    // role="checkbox" is placed on the label (when icon present) so click detection
    // tools can find a visible element with that role.
    let mut input = RawHtmlEl::new("input")
        .attr("type", "checkbox")
        .style("position", "absolute")
        .style("opacity", "0")
        .style("width", "0")
        .style("height", "0")
        .style("margin", "0")
        .style("padding", "0");

    // Reactively bind the `checked` property to the cell value.
    if let Some(&checked_cell) = checked {
        if let Some(ref ics) = instance.item_cell_store {
            let ics = ics.clone();
            let item_idx = ctx.item_idx;
            let cell_id = checked_cell.0;
            let initial_val = ics.get_value(item_idx, cell_id);
            let initial = initial_val != 0.0;
            if initial {
                input = input.attr("checked", "");
            }
            input = input.after_insert(move |el: web_sys::HtmlElement| {
                let handle = Task::start_droppable(
                    ics.get_signal(item_idx, cell_id)
                        .for_each_sync(move |val| {
                            let is_checked = val != 0.0;
                            if let Some(inp) = el.dyn_ref::<web_sys::HtmlInputElement>() {
                                inp.set_checked(is_checked);
                            }
                        })
                );
                std::mem::forget(handle);
            });
        }
    }

    if let Some(&icon_cell) = icon {
        let icon_el = build_item_element(program, instance, ctx, icon_cell);
        // Use <div> instead of <label> to avoid native label click-forwarding
        // which interferes with the event wrapper from attach_item_events.
        let mut wrapper = RawHtmlEl::new("div")
            .attr("role", "checkbox")
            .style("display", "flex")
            .style("align-items", "center")
            .style("cursor", "pointer")
            .style("position", "relative");
        // Reactively set aria-checked for accessibility and test tooling.
        if let Some(&checked_cell) = checked {
            if let Some(ref ics) = instance.item_cell_store {
                let ics2 = ics.clone();
                let item_idx2 = ctx.item_idx;
                let cell_id2 = checked_cell.0;
                let initial2 = ics2.get_value(item_idx2, cell_id2) != 0.0;
                wrapper = wrapper.attr("aria-checked", if initial2 { "true" } else { "false" });
                wrapper = wrapper.after_insert(move |el: web_sys::HtmlElement| {
                    let handle = Task::start_droppable(
                        ics2.get_signal(item_idx2, cell_id2)
                            .for_each_sync(move |val| {
                                let _ = el.set_attribute("aria-checked", if val != 0.0 { "true" } else { "false" });
                            })
                    );
                    std::mem::forget(handle);
                });
            }
        }
        wrapper = apply_styles_item(wrapper, style, program, instance, ctx);
        wrapper = wrapper
            .child(input)
            .child(icon_el);
        wrapper.into_raw_unchecked()
    } else {
        // No icon — visible checkbox input with role for click detection.
        input = input.attr("role", "checkbox");
        input = apply_styles_item(input, style, program, instance, ctx);
        input.into_raw_unchecked()
    }
}

/// Build a per-item Container element.
fn build_item_container(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    ctx: &ItemContext,
    child: CellId,
    style: &IrExpr,
) -> RawElOrText {
    let child_el = build_item_element(program, instance, ctx, child);
    let mut raw = RawHtmlEl::new("div").child(child_el);
    raw = apply_styles_item(raw, style, program, instance, ctx);
    raw.into_raw_unchecked()
}

/// Build a per-item Label element.
fn build_item_label(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    ctx: &ItemContext,
    label: &IrExpr,
    style: &IrExpr,
    _links: &[(String, EventId)],
) -> RawElOrText {
    let content: RawElOrText = match label {
        IrExpr::TextConcat(segments) => {
            build_item_text_from_segments(instance, ctx, segments)
        }
        IrExpr::Constant(IrValue::Text(t)) => {
            zoon::Text::new(t.clone()).unify()
        }
        IrExpr::CellRead(cell) => {
            build_item_element(program, instance, ctx, *cell)
        }
        _ => {
            let text = eval_static_text(label);
            zoon::Text::new(text).unify()
        }
    };
    let mut raw = RawHtmlEl::new("span").child(content);
    raw = apply_styles_item(raw, style, program, instance, ctx);
    raw.into_raw_unchecked()
}

/// Build a per-item Stack element.
fn build_item_stack(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    ctx: &ItemContext,
    layers_cell: CellId,
    style: &IrExpr,
) -> RawElOrText {
    let children = collect_item_stripe_children(program, instance, ctx, layers_cell);
    let mut raw = RawHtmlEl::new("div").style("position", "relative");
    raw = apply_styles_item(raw, style, program, instance, ctx);
    raw = raw.children(children);
    raw.into_raw_unchecked()
}

/// Build a per-item conditional element (WHEN/WHILE) with per-item signal routing.
fn build_item_conditional_element(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    ctx: &ItemContext,
    source: CellId,
    arms: &[(IrPattern, IrExpr)],
) -> RawElOrText {
    let tag_table = &program.tag_table;
    let arm_elements: Vec<(ArmMatcher, RawElOrText)> = arms.iter().map(|(pattern, body)| {
        let matcher = pattern_to_matcher(pattern, tag_table);
        let element = match body {
            IrExpr::CellRead(cell) => {
                if is_no_element(program, *cell) {
                    RawHtmlEl::new("span").style("display", "none").into_raw_unchecked()
                } else {
                    build_item_element(program, instance, ctx, *cell)
                }
            }
            IrExpr::Constant(IrValue::Tag(t)) if t == "NoElement" => {
                RawHtmlEl::new("span").style("display", "none").into_raw_unchecked()
            }
            IrExpr::TextConcat(segments) => {
                build_item_text_from_segments(instance, ctx, segments)
            }
            _ => {
                let text = eval_static_text(body);
                if text.is_empty() {
                    if ctx.is_template_cell(source) {
                        build_item_reactive_text(instance, ctx, source)
                    } else {
                        build_reactive_text(instance, source)
                    }
                } else {
                    zoon::Text::new(text).unify()
                }
            }
        };
        (matcher, element)
    }).collect();

    if arm_elements.len() == 1 {
        return arm_elements.into_iter().next().unwrap().1;
    }

    let matchers: Vec<ArmMatcher> = arm_elements.iter().map(|(m, _)| m.clone()).collect();
    let elements: Vec<RawElOrText> = arm_elements.into_iter().map(|(_, e)| e).collect();

    // Use per-item signal if source is template-scoped, otherwise global.
    let is_item_source = ctx.is_template_cell(source);
    let source_id = source.0;

    let mut result_children: Vec<RawElOrText> = Vec::new();
    for (i, element) in elements.into_iter().enumerate() {
        let matchers_clone = matchers.clone();

        // Determine initial visibility.
        let (initial_val, initial_text) = if is_item_source {
            if let Some(ref ics) = instance.item_cell_store {
                (ics.get_value(ctx.item_idx, source_id), ics.get_text(ctx.item_idx, source_id))
            } else {
                (instance.cell_store.get_cell_value(source_id), instance.cell_store.get_cell_text(source_id))
            }
        } else {
            (instance.cell_store.get_cell_value(source_id), instance.cell_store.get_cell_text(source_id))
        };
        let initial_matched = find_matching_arm_idx(&matchers, initial_val, &initial_text);
        let initially_visible = initial_matched == Some(i);

        if is_item_source {
            let ics = instance.item_cell_store.clone().unwrap();
            let item_idx = ctx.item_idx;
            let matchers_inner = matchers_clone.clone();
            let wrapper = RawHtmlEl::new("div")
                .style("display", if initially_visible { "contents" } else { "none" })
                .child(element)
                .after_insert(move |el: web_sys::HtmlElement| {
                    let val = ics.get_value(item_idx, source_id);
                    let cell_text = ics.get_text(item_idx, source_id);
                    let matched = find_matching_arm_idx(&matchers_inner, val, &cell_text);
                    let _ = el.style().set_property("display", if matched == Some(i) { "contents" } else { "none" });

                    let ics_inner = ics.clone();
                    let matchers_watch = matchers_inner.clone();
                    let handle = Task::start_droppable(
                        ics.get_signal(item_idx, source_id)
                            .for_each_sync(move |_val| {
                                let cell_text = ics_inner.get_text(item_idx, source_id);
                                let val = ics_inner.get_value(item_idx, source_id);
                                let matched = find_matching_arm_idx(&matchers_watch, val, &cell_text);
                                let visible = matched == Some(i);
                                let _ = el.style().set_property("display", if visible { "contents" } else { "none" });
                                // When becoming visible, focus any autofocus input inside.
                                if visible {
                                    focus_autofocus_child(&el);
                                }
                            })
                    );
                    std::mem::forget(handle);
                });
            result_children.push(wrapper.into_raw_unchecked());
        } else {
            // Global source — same as build_conditional_element.
            let store = instance.cell_store.clone();
            let matchers_inner = matchers_clone;
            let wrapper = RawHtmlEl::new("div")
                .style("display", if initially_visible { "contents" } else { "none" })
                .child(element)
                .after_insert(move |el: web_sys::HtmlElement| {
                    let val = store.get_cell_value(source_id);
                    let cell_text = store.get_cell_text(source_id);
                    let matched = find_matching_arm_idx(&matchers_inner, val, &cell_text);
                    let _ = el.style().set_property("display", if matched == Some(i) { "contents" } else { "none" });

                    let store_inner = store.clone();
                    let matchers_watch = matchers_inner.clone();
                    let handle = Task::start_droppable(
                        store.get_cell_signal(source_id)
                            .for_each_sync(move |val| {
                                let cell_text = store_inner.get_cell_text(source_id);
                                let matched = find_matching_arm_idx(&matchers_watch, val, &cell_text);
                                let visible = matched == Some(i);
                                let _ = el.style().set_property("display", if visible { "contents" } else { "none" });
                                // When becoming visible, focus any autofocus input inside.
                                if visible {
                                    focus_autofocus_child(&el);
                                }
                            })
                    );
                    std::mem::forget(handle);
                });
            result_children.push(wrapper.into_raw_unchecked());
        }
    }

    RawHtmlEl::new("div")
        .style("display", "contents")
        .children(result_children)
        .into_raw_unchecked()
}

/// Resolve a color expression to a CSS color string.
fn resolve_color(expr: &IrExpr) -> Option<String> {
    resolve_color_full(expr)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Find the root CellId from the Document node.
fn find_document_root(program: &IrProgram) -> Option<CellId> {
    for node in &program.nodes {
        if let IrNode::Document { root } = node {
            return Some(*root);
        }
    }
    None
}

/// Find the IrNode that defines a given cell.
fn find_node_for_cell(program: &IrProgram, cell: CellId) -> Option<&IrNode> {
    for node in &program.nodes {
        match node {
            IrNode::Derived { cell: c, .. }
            | IrNode::Hold { cell: c, .. }
            | IrNode::Latest { target: c, .. }
            | IrNode::Then { cell: c, .. }
            | IrNode::When { cell: c, .. }
            | IrNode::While { cell: c, .. }
            | IrNode::MathSum { cell: c, .. }
            | IrNode::PipeThrough { cell: c, .. }
            | IrNode::TextInterpolation { cell: c, .. }
            | IrNode::CustomCall { cell: c, .. }
            | IrNode::Element { cell: c, .. }
            | IrNode::ListAppend { cell: c, .. }
            | IrNode::ListClear { cell: c, .. }
            | IrNode::ListCount { cell: c, .. }
            | IrNode::ListMap { cell: c, .. }
            | IrNode::ListRemove { cell: c, .. }
            | IrNode::ListRetain { cell: c, .. }
            | IrNode::ListIsEmpty { cell: c, .. }
            | IrNode::RouterGoTo { cell: c, .. }
            | IrNode::TextTrim { cell: c, .. }
            | IrNode::TextIsNotEmpty { cell: c, .. }
            | IrNode::HoldLoop { cell: c, .. } => {
                if *c == cell {
                    return Some(node);
                }
            }
            IrNode::Document { root } => {
                if *root == cell {
                    return Some(node);
                }
            }
            IrNode::Timer { .. } => {}
        }
    }
    None
}

/// Extract placeholder text from a placeholder expression.
/// Handles both simple text and complex objects like `[style: ..., text: TEXT { ... }]`.
fn extract_placeholder_text(expr: &IrExpr, program: &IrProgram) -> String {
    match expr {
        IrExpr::Constant(IrValue::Text(t)) => t.clone(),
        IrExpr::TextConcat(segs) => {
            segs.iter().map(|s| match s {
                TextSegment::Literal(t) => t.clone(),
                _ => String::new(),
            }).collect()
        }
        IrExpr::ObjectConstruct(fields) => {
            // Look for a "text" field in the object.
            for (name, val) in fields {
                if name == "text" {
                    return extract_placeholder_text(val, program);
                }
            }
            String::new()
        }
        IrExpr::CellRead(cell) => {
            // Placeholder lowered as cell store — reconstruct and look for "text" field.
            let fields = reconstruct_object_fields(program, *cell);
            for (name, val) in &fields {
                if name == "text" {
                    return extract_placeholder_text(val, program);
                }
            }
            String::new()
        }
        _ => eval_static_text(expr),
    }
}

/// Evaluate a static text expression to a String.
fn eval_static_text(expr: &IrExpr) -> String {
    match expr {
        IrExpr::Constant(IrValue::Text(t)) => t.clone(),
        IrExpr::Constant(IrValue::Number(n)) => format_number(*n),
        IrExpr::Constant(IrValue::Tag(t)) => t.clone(),
        IrExpr::TextConcat(segs) => {
            segs.iter()
                .map(|s| match s {
                    TextSegment::Literal(t) => t.clone(),
                    _ => String::new(),
                })
                .collect()
        }
        _ => String::new(),
    }
}

/// Resolve an IrExpr through CellRead chains to find the underlying static text.
/// Follows Derived { expr: CellRead(...) } and Derived { expr: TextConcat/Constant }
/// through the program's node list.
fn resolve_static_text(program: &IrProgram, expr: &IrExpr) -> String {
    resolve_static_text_depth(program, expr, 0)
}

fn resolve_static_text_depth(program: &IrProgram, expr: &IrExpr, depth: u32) -> String {
    if depth > 20 { return String::new(); }
    match expr {
        IrExpr::CellRead(cell) => {
            // Find the node for this cell and extract its expression.
            if let Some(node) = find_node_for_cell(program, *cell) {
                match node {
                    IrNode::Derived { expr: inner, .. } => resolve_static_text_depth(program, inner, depth + 1),
                    IrNode::TextInterpolation { parts, .. } => {
                        parts.iter().map(|seg| match seg {
                            TextSegment::Literal(t) => t.clone(),
                            _ => String::new(),
                        }).collect()
                    }
                    IrNode::When { source, arms, .. } | IrNode::While { source, arms, .. } => {
                        resolve_when_text_statically(program, *source, arms, depth + 1)
                    }
                    IrNode::PipeThrough { source, .. } => {
                        resolve_static_text_depth(program, &IrExpr::CellRead(*source), depth + 1)
                    }
                    _ => String::new(),
                }
            } else {
                String::new()
            }
        }
        _ => eval_static_text(expr),
    }
}

/// Try to resolve a WHEN/WHILE pattern match statically.
fn resolve_when_text_statically(
    program: &IrProgram,
    source: CellId,
    arms: &[(IrPattern, IrExpr)],
    depth: u32,
) -> String {
    if depth > 20 { return String::new(); }
    let source_value = resolve_cell_constant(program, source, depth + 1);

    for (pattern, body) in arms {
        let matches = match (pattern, &source_value) {
            (IrPattern::Tag(t), Some(ConstValue::Tag(v))) => t == v,
            (IrPattern::Number(n), Some(ConstValue::Number(v))) => *n == *v,
            (IrPattern::Wildcard | IrPattern::Binding(_), _) => true,
            _ => false,
        };
        if matches {
            return resolve_static_text_depth(program, body, depth + 1);
        }
    }
    String::new()
}

enum ConstValue {
    Tag(String),
    Number(f64),
    Text(String),
}

/// Resolve a cell to its constant value (if it has one).
fn resolve_cell_constant(program: &IrProgram, cell: CellId, depth: u32) -> Option<ConstValue> {
    if depth > 20 { return None; }
    let node = find_node_for_cell(program, cell)?;
    match node {
        IrNode::Derived { expr, .. } => resolve_expr_constant(program, expr, depth + 1),
        _ => None,
    }
}

fn resolve_expr_constant(program: &IrProgram, expr: &IrExpr, depth: u32) -> Option<ConstValue> {
    if depth > 20 { return None; }
    match expr {
        IrExpr::Constant(IrValue::Tag(t)) => Some(ConstValue::Tag(t.clone())),
        IrExpr::Constant(IrValue::Number(n)) => Some(ConstValue::Number(*n)),
        IrExpr::Constant(IrValue::Text(t)) => Some(ConstValue::Text(t.clone())),
        IrExpr::CellRead(cell) => resolve_cell_constant(program, *cell, depth + 1),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Style application
// ---------------------------------------------------------------------------

/// Apply Boon style properties as CSS to a RawHtmlEl.
fn apply_styles(
    el: RawHtmlEl<web_sys::HtmlElement>,
    style: &IrExpr,
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
) -> RawHtmlEl<web_sys::HtmlElement> {
    apply_styles_inner(el, style, program, instance, None)
}

fn apply_styles_item(
    el: RawHtmlEl<web_sys::HtmlElement>,
    style: &IrExpr,
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    ctx: &ItemContext,
) -> RawHtmlEl<web_sys::HtmlElement> {
    apply_styles_inner(el, style, program, instance, Some(ctx))
}

fn apply_styles_inner(
    mut el: RawHtmlEl<web_sys::HtmlElement>,
    style: &IrExpr,
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    item_ctx: Option<&ItemContext>,
) -> RawHtmlEl<web_sys::HtmlElement> {
    // Style can be ObjectConstruct (direct) or CellRead (when nested objects
    // caused the lowerer to use the object-store pattern). For CellRead, we
    // reconstruct the field expressions from the IR node tree.
    let reconstructed;
    let fields: &[(String, IrExpr)] = match style {
        IrExpr::ObjectConstruct(fields) => fields,
        IrExpr::CellRead(cell) => {
            reconstructed = reconstruct_object_fields(program, *cell);
            &reconstructed
        }
        IrExpr::Constant(IrValue::Void) => return el,
        _ => return el,
    };

    for (name, value) in fields {
        match name.as_str() {
            "width" => {
                if let Some(css) = dimension_to_css(value) {
                    el = el.style("width", &css);
                }
                // Fill width also needs flex-grow for inline-flex parents.
                if matches!(value, IrExpr::Constant(IrValue::Tag(t)) if t == "Fill") {
                    el = el.style("flex-grow", "1");
                }
            }
            "height" => {
                if let Some(css) = dimension_to_css(value) {
                    el = el.style("height", &css);
                }
                // Fill height also needs flex-grow for inline-flex parents.
                if matches!(value, IrExpr::Constant(IrValue::Tag(t)) if t == "Fill") {
                    el = el.style("flex-grow", "1");
                }
            }
            "size" => {
                if let Some(css) = dimension_to_css(value) {
                    el = el.style("width", &css);
                    el = el.style("height", &css);
                }
            }
            "padding" => {
                el = apply_padding(el, value);
            }
            "font" => {
                el = apply_font(el, value, program);
            }
            "background" => {
                el = apply_background(el, value, program, instance, item_ctx);
            }
            "align" => {
                el = apply_align(el, value);
            }
            "line_height" => {
                if let IrExpr::Constant(IrValue::Number(n)) = value {
                    el = el.style("line-height", &format!("{}", n));
                }
            }
            "font_smoothing" => {
                if let IrExpr::Constant(IrValue::Tag(t)) = value {
                    if t == "Antialiased" {
                        // Use raw DOM API because dominator's .style() auto-adds
                        // vendor prefixes, which creates invalid double-prefixed
                        // names like "-webkit--moz-osx-font-smoothing".
                        el = el.after_insert(|element: web_sys::HtmlElement| {
                            let style = element.style();
                            let _ = style.set_property("-webkit-font-smoothing", "antialiased");
                            let _ = style.set_property("-moz-osx-font-smoothing", "grayscale");
                        });
                    }
                }
            }
            "rounded_corners" => {
                if let IrExpr::Constant(IrValue::Number(n)) = value {
                    el = el.style("border-radius", &format!("{}px", n));
                }
            }
            "borders" => {
                el = apply_borders(el, value);
            }
            "shadows" => {
                el = apply_shadows(el, value, program);
            }
            "outline" => {
                el = apply_outline(el, value, program, instance);
            }
            "transform" => {
                el = apply_transform(el, value, program);
            }
            _ => {}
        }
    }
    el
}

/// Reconstruct object fields from a cell store (namespace cell).
/// When an object was lowered with the object-store pattern (because it had
/// nested objects), its fields are stored as separate cells. This function
/// looks up `cell_field_cells` for the cell and extracts each field's expression
/// from the corresponding Derived node.
///
/// When a field is itself a namespace cell (Derived with Void + has cell_field_cells),
/// recursively reconstruct it as ObjectConstruct so style handlers can process it.
fn reconstruct_object_fields(program: &IrProgram, cell: CellId) -> Vec<(String, IrExpr)> {
    let mut fields = Vec::new();
    if let Some(field_map) = program.cell_field_cells.get(&cell) {
        for (name, field_cell) in field_map {
            if let Some(node) = find_node_for_cell(program, *field_cell) {
                match node {
                    IrNode::Derived { expr, .. } => {
                        // Check if this field is itself a namespace cell (nested object
                        // lowered via lower_object_store). In that case, expr is Void
                        // and the actual fields are in cell_field_cells.
                        if matches!(expr, IrExpr::Constant(IrValue::Void))
                            && program.cell_field_cells.contains_key(field_cell)
                        {
                            let inner = reconstruct_object_fields(program, *field_cell);
                            fields.push((name.clone(), IrExpr::ObjectConstruct(inner)));
                        } else {
                            fields.push((name.clone(), expr.clone()));
                        }
                    }
                    // Non-Derived nodes (When, While, Hold, etc.) — expose as
                    // CellRead so style handlers can process them reactively.
                    _ => {
                        fields.push((name.clone(), IrExpr::CellRead(*field_cell)));
                    }
                }
            }
        }
    }
    fields
}

/// Convert a Boon dimension value to CSS.
fn dimension_to_css(expr: &IrExpr) -> Option<String> {
    match expr {
        IrExpr::Constant(IrValue::Number(n)) => Some(format!("{}px", n)),
        IrExpr::Constant(IrValue::Tag(t)) if t == "Fill" => Some("100%".to_string()),
        _ => None,
    }
}

/// Apply padding from a Boon padding object.
fn apply_padding(
    mut el: RawHtmlEl<web_sys::HtmlElement>,
    value: &IrExpr,
) -> RawHtmlEl<web_sys::HtmlElement> {
    if let IrExpr::ObjectConstruct(fields) = value {
        let mut top = None;
        let mut bottom = None;
        let mut left = None;
        let mut right = None;

        for (name, val) in fields {
            if let IrExpr::Constant(IrValue::Number(n)) = val {
                match name.as_str() {
                    "top" => top = Some(*n),
                    "bottom" => bottom = Some(*n),
                    "left" => left = Some(*n),
                    "right" => right = Some(*n),
                    "row" => { top = top.or(Some(*n)); bottom = bottom.or(Some(*n)); }
                    "column" => { left = left.or(Some(*n)); right = right.or(Some(*n)); }
                    _ => {}
                }
            }
        }

        let t = top.unwrap_or(0.0);
        let r = right.unwrap_or(0.0);
        let b = bottom.unwrap_or(0.0);
        let l = left.unwrap_or(0.0);
        if t != 0.0 || r != 0.0 || b != 0.0 || l != 0.0 {
            el = el.style("padding", &format!("{}px {}px {}px {}px", t, r, b, l));
        }
    }
    el
}

/// Apply font properties.
fn apply_font(
    mut el: RawHtmlEl<web_sys::HtmlElement>,
    value: &IrExpr,
    program: &IrProgram,
) -> RawHtmlEl<web_sys::HtmlElement> {
    if let IrExpr::ObjectConstruct(fields) = value {
        for (name, val) in fields {
            match name.as_str() {
                "size" => {
                    if let IrExpr::Constant(IrValue::Number(n)) = val {
                        el = el.style("font-size", &format!("{}px", n));
                    }
                }
                "color" => {
                    if let Some(css) = resolve_color(val) {
                        el = el.style("color", &css);
                    }
                }
                "weight" => {
                    if let IrExpr::Constant(IrValue::Tag(t)) = val {
                        let w = match t.as_str() {
                            "ExtraLight" => "200",
                            "Light" => "300",
                            "Regular" | "Normal" => "400",
                            "Medium" => "500",
                            "SemiBold" => "600",
                            "Bold" => "700",
                            "ExtraBold" => "800",
                            _ => "400",
                        };
                        el = el.style("font-weight", w);
                    }
                }
                "family" => {
                    if let Some(family) = resolve_font_family(val, program) {
                        el = el.style("font-family", &family);
                    }
                }
                "align" => {
                    if let IrExpr::Constant(IrValue::Tag(t)) = val {
                        let a = match t.as_str() {
                            "Center" => "center",
                            "Left" | "Start" => "left",
                            "Right" | "End" => "right",
                            _ => "left",
                        };
                        el = el.style("text-align", a);
                    }
                }
                "style" => {
                    if let IrExpr::Constant(IrValue::Tag(t)) = val {
                        if t == "Italic" {
                            el = el.style("font-style", "italic");
                        }
                    }
                }
                _ => {}
            }
        }
    }
    el
}

/// Resolve font family to CSS value.
fn resolve_font_family(expr: &IrExpr, program: &IrProgram) -> Option<String> {
    // Font family could be a ListConstruct of text values, or a CellRead to a ListConstruct.
    let items = match expr {
        IrExpr::ListConstruct(items) => items.clone(),
        IrExpr::CellRead(cell) => {
            if let Some(IrNode::Derived { expr: IrExpr::ListConstruct(items), .. }) = find_node_for_cell(program, *cell) {
                items.clone()
            } else {
                return None;
            }
        }
        _ => return None,
    };

    let families: Vec<String> = items.iter().filter_map(|item| {
        match item {
            IrExpr::Constant(IrValue::Text(t)) => Some(format!("\"{}\"", t)),
            IrExpr::Constant(IrValue::Tag(t)) => {
                match t.as_str() {
                    "SansSerif" => Some("sans-serif".to_string()),
                    "Serif" => Some("serif".to_string()),
                    "Monospace" => Some("monospace".to_string()),
                    _ => Some(t.clone()),
                }
            }
            IrExpr::CellRead(cell) => {
                if let Some(node) = find_node_for_cell(program, *cell) {
                    match node {
                        IrNode::TextInterpolation { parts, .. } | IrNode::Derived { expr: IrExpr::TextConcat(parts), .. } => {
                            let text: String = parts.iter().map(|p| match p {
                                TextSegment::Literal(s) => s.clone(),
                                _ => String::new(),
                            }).collect();
                            if text.is_empty() { None } else { Some(format!("\"{}\"", text)) }
                        }
                        IrNode::Derived { expr: IrExpr::Constant(IrValue::Text(t)), .. } => {
                            Some(format!("\"{}\"", t))
                        }
                        IrNode::Derived { expr: IrExpr::Constant(IrValue::Tag(t)), .. } => {
                            match t.as_str() {
                                "SansSerif" => Some("sans-serif".to_string()),
                                _ => Some(t.clone()),
                            }
                        }
                        _ => None,
                    }
                } else {
                    None
                }
            }
            _ => None,
        }
    }).collect();

    if families.is_empty() { None } else { Some(families.join(", ")) }
}

/// Apply background properties.
fn apply_background(
    mut el: RawHtmlEl<web_sys::HtmlElement>,
    value: &IrExpr,
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    item_ctx: Option<&ItemContext>,
) -> RawHtmlEl<web_sys::HtmlElement> {
    let reconstructed;
    let fields: &[(String, IrExpr)] = match value {
        IrExpr::ObjectConstruct(fields) => fields,
        IrExpr::CellRead(cell) => {
            reconstructed = reconstruct_object_fields(program, *cell);
            &reconstructed
        }
        _ => return el,
    };
    for (name, val) in fields {
        match name.as_str() {
            "color" => {
                if let Some(css) = resolve_color_full(val) {
                    el = el.style("background-color", &css);
                }
            }
            "url" => {
                // Try static text first, then follow CellRead chains.
                let url = eval_static_text(val);
                if !url.is_empty() {
                    el = el.style("background-image", &format!("url({})", url));
                    el = el.style("background-size", "contain");
                    el = el.style("background-repeat", "no-repeat");
                } else {
                    let url = resolve_static_text(program, val);
                    if !url.is_empty() {
                        el = el.style("background-image", &format!("url({})", url));
                        el = el.style("background-size", "contain");
                        el = el.style("background-repeat", "no-repeat");
                    } else {
                        // Reactive URL: the value is a CellRead to a WHEN/WHILE node
                        // whose arms produce text (e.g. SVG data URIs for checkbox icons).
                        el = apply_reactive_background_url(el, val, program, instance, item_ctx);
                    }
                }
            }
            _ => {}
        }
    }
    el
}

/// Set up a reactive background-image from a WHEN/WHILE expression.
/// Collects text for each arm statically, watches the source cell, and
/// switches the CSS property when the source value changes.
fn apply_reactive_background_url(
    mut el: RawHtmlEl<web_sys::HtmlElement>,
    url_expr: &IrExpr,
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    item_ctx: Option<&ItemContext>,
) -> RawHtmlEl<web_sys::HtmlElement> {
    if let IrExpr::CellRead(cell) = url_expr {
        if let Some(node) = find_node_for_cell(program, *cell) {
            if let IrNode::When { source, arms, .. } | IrNode::While { source, arms, .. } = node {
                // Collect (pattern_f64, url_text) for each arm.
                let mut arm_urls: Vec<(f64, String)> = Vec::new();
                for (pat, body) in arms {
                    let f64_val = pattern_to_f64(pat, program);
                    let text = resolve_static_text(program, body);
                    if !text.is_empty() {
                        arm_urls.push((f64_val, text));
                    }
                }
                if arm_urls.is_empty() {
                    return el;
                }

                let source_id = source.0;
                el = el.style("background-size", "contain");
                el = el.style("background-repeat", "no-repeat");

                // Per-item case: source cell is a template cell, use ItemCellStore signals.
                if let (Some(ctx), Some(ics)) = (item_ctx, &instance.item_cell_store) {
                    if ics.is_template_cell(source_id) {
                        let item_idx = ctx.item_idx;
                        let initial_val = ics.get_value(item_idx, source_id);
                        for (pat_val, url) in &arm_urls {
                            if initial_val == *pat_val {
                                el = el.style("background-image", &format!("url({})", url));
                                break;
                            }
                        }
                        let ics_clone = ics.clone();
                        el = el.after_insert(move |element: web_sys::HtmlElement| {
                            let handle = Task::start_droppable(
                                ics_clone.get_signal(item_idx, source_id)
                                    .for_each_sync(move |val| {
                                        for (pat_val, url) in &arm_urls {
                                            if val == *pat_val {
                                                let _ = element.style().set_property(
                                                    "background-image",
                                                    &format!("url({})", url),
                                                );
                                                break;
                                            }
                                        }
                                    })
                            );
                            std::mem::forget(handle);
                        });
                        return el;
                    }
                }

                // Global cell: use cell_store.
                let initial_val = instance.cell_store.get_cell_value(source_id);
                for (pat_val, url) in &arm_urls {
                    if initial_val == *pat_val {
                        el = el.style("background-image", &format!("url({})", url));
                        break;
                    }
                }
                let store = instance.cell_store.clone();
                el = el.after_insert(move |element: web_sys::HtmlElement| {
                    let handle = Task::start_droppable(
                        store.get_cell_signal(source_id)
                            .for_each_sync(move |val| {
                                for (pat_val, url) in &arm_urls {
                                    if val == *pat_val {
                                        let _ = element.style().set_property(
                                            "background-image",
                                            &format!("url({})", url),
                                        );
                                        break;
                                    }
                                }
                            })
                    );
                    std::mem::forget(handle);
                });
            }
        }
    }
    el
}

/// Apply align properties (maps to flex alignment).
fn apply_align(
    mut el: RawHtmlEl<web_sys::HtmlElement>,
    value: &IrExpr,
) -> RawHtmlEl<web_sys::HtmlElement> {
    if let IrExpr::ObjectConstruct(fields) = value {
        for (name, val) in fields {
            if let IrExpr::Constant(IrValue::Tag(t)) = val {
                let css_val = match t.as_str() {
                    "Center" => "center",
                    "Start" | "Left" | "Top" => "flex-start",
                    "End" | "Right" | "Bottom" => "flex-end",
                    _ => "flex-start",
                };
                match name.as_str() {
                    "row" => { el = el.style("align-items", css_val); }
                    "column" => { el = el.style("justify-content", css_val); }
                    _ => {}
                }
            }
        }
    }
    el
}

/// Apply border properties.
fn apply_borders(
    mut el: RawHtmlEl<web_sys::HtmlElement>,
    value: &IrExpr,
) -> RawHtmlEl<web_sys::HtmlElement> {
    if let IrExpr::ObjectConstruct(fields) = value {
        for (name, val) in fields {
            if let IrExpr::ObjectConstruct(border_fields) = val {
                let color = border_fields.iter()
                    .find(|(n, _)| n == "color")
                    .and_then(|(_, v)| resolve_color_full(v))
                    .unwrap_or_else(|| "currentColor".to_string());
                let width = border_fields.iter()
                    .find(|(n, _)| n == "width")
                    .and_then(|(_, v)| if let IrExpr::Constant(IrValue::Number(n)) = v { Some(*n) } else { None })
                    .unwrap_or(1.0);
                let border_css = format!("{}px solid {}", width, color);
                match name.as_str() {
                    "top" => { el = el.style("border-top", &border_css); }
                    "bottom" => { el = el.style("border-bottom", &border_css); }
                    "left" => { el = el.style("border-left", &border_css); }
                    "right" => { el = el.style("border-right", &border_css); }
                    _ => {}
                }
            }
        }
    }
    el
}

/// Apply shadow properties.
fn apply_shadows(
    mut el: RawHtmlEl<web_sys::HtmlElement>,
    value: &IrExpr,
    program: &IrProgram,
) -> RawHtmlEl<web_sys::HtmlElement> {
    // Shadows can be a ListConstruct directly, or a CellRead to a Derived
    // node holding a ListConstruct (when the style used the object-store pattern).
    let resolved;
    let items = match value {
        IrExpr::ListConstruct(items) => items,
        IrExpr::CellRead(cell) => {
            if let Some(IrNode::Derived { expr: IrExpr::ListConstruct(items), .. }) = find_node_for_cell(program, *cell) {
                resolved = items.clone();
                &resolved
            } else {
                return el;
            }
        }
        _ => return el,
    };

    let shadows: Vec<String> = items.iter().filter_map(|item| {
        // Items may be ObjectConstruct (direct) or CellRead (when force_object_store
        // was active during lowering). Handle both by reconstructing if needed.
        let reconstructed;
        let fields: &[(String, IrExpr)] = match item {
            IrExpr::ObjectConstruct(f) => f,
            IrExpr::CellRead(cell) => {
                reconstructed = reconstruct_object_fields(program, *cell);
                &reconstructed
            }
            _ => return None,
        };

        let mut x = 0.0f64;
        let mut y = 0.0f64;
        let mut blur = 0.0f64;
        let mut spread = 0.0f64;
        let mut color = "rgba(0,0,0,0.2)".to_string();
        let mut inset = false;

        for (name, val) in fields {
            match name.as_str() {
                "x" => if let IrExpr::Constant(IrValue::Number(n)) = val { x = *n; },
                "y" => if let IrExpr::Constant(IrValue::Number(n)) = val { y = *n; },
                "blur" => if let IrExpr::Constant(IrValue::Number(n)) = val { blur = *n; },
                "spread" => if let IrExpr::Constant(IrValue::Number(n)) = val { spread = *n; },
                "color" => { color = resolve_color_full(val).unwrap_or(color); },
                "direction" => {
                    if let IrExpr::Constant(IrValue::Tag(t)) = val {
                        if t == "Inwards" { inset = true; }
                    }
                }
                _ => {}
            }
        }

        let inset_str = if inset { "inset " } else { "" };
        Some(format!("{}{}px {}px {}px {}px {}", inset_str, x, y, blur, spread, color))
    }).collect();

    if !shadows.is_empty() {
        el = el.style("box-shadow", &shadows.join(", "));
    }
    el
}

/// Convert an IrPattern to the f64 value the WASM cell would hold when that arm matches.
fn pattern_to_f64(pat: &IrPattern, program: &IrProgram) -> f64 {
    match pat {
        IrPattern::Number(n) => *n,
        IrPattern::Tag(tag) => {
            program.tag_table.iter()
                .position(|t| t == tag)
                .map(|i| (i + 1) as f64)
                .unwrap_or(0.0)
        }
        IrPattern::Text(_) | IrPattern::Wildcard | IrPattern::Binding(_) => 0.0,
    }
}

/// Compute outline CSS from a static IrExpr. Returns (outline_css, box_shadow_css).
fn resolve_outline_css(value: &IrExpr) -> (String, String) {
    match value {
        IrExpr::Constant(IrValue::Tag(t)) if t == "NoOutline" => {
            ("none".to_string(), "none".to_string())
        }
        IrExpr::ObjectConstruct(fields) => {
            let color = fields.iter()
                .find(|(n, _)| n == "color")
                .and_then(|(_, v)| resolve_color_full(v))
                .unwrap_or_else(|| "currentColor".to_string());
            let side = fields.iter()
                .find(|(n, _)| n == "side")
                .and_then(|(_, v)| if let IrExpr::Constant(IrValue::Tag(t)) = v { Some(t.as_str()) } else { None });
            if side == Some("Inner") {
                ("none".to_string(), format!("inset 0 0 0 1px {}", color))
            } else {
                (format!("1px solid {}", color), "none".to_string())
            }
        }
        _ => ("none".to_string(), "none".to_string()),
    }
}

/// Recursively collect all (f64_pattern_value, outline_css, box_shadow_css) from WHILE/WHEN arms.
/// For nested CellRead bodies, follows to inner WHILE/WHEN nodes and collects their arms too,
/// using the SOURCE cell's signal for each level.
fn collect_outline_arm_css(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    arms: &[(IrPattern, IrExpr)],
    source: CellId,
    result: &mut Vec<(CellId, f64, String, String)>,
) {
    for (pat, body) in arms {
        let f64_val = pattern_to_f64(pat, program);
        match body {
            IrExpr::CellRead(inner_cell) => {
                // Nested WHILE/WHEN — recurse.
                if let Some(node) = find_node_for_cell(program, *inner_cell) {
                    match node {
                        IrNode::While { source: inner_source, arms: inner_arms, .. }
                        | IrNode::When { source: inner_source, arms: inner_arms, .. } => {
                            collect_outline_arm_css(program, instance, inner_arms, *inner_source, result);
                        }
                        IrNode::Derived { expr, .. } => {
                            // Check if this is a namespace cell (Void + cell_field_cells)
                            // from the object-store pattern. If so, reconstruct and resolve.
                            if matches!(expr, IrExpr::Constant(IrValue::Void))
                                && program.cell_field_cells.contains_key(inner_cell)
                            {
                                let fields = reconstruct_object_fields(program, *inner_cell);
                                let obj = IrExpr::ObjectConstruct(fields);
                                let (outline, shadow) = resolve_outline_css(&obj);
                                result.push((source, f64_val, outline, shadow));
                            } else {
                                let (outline, shadow) = resolve_outline_css(expr);
                                result.push((source, f64_val, outline, shadow));
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {
                let (outline, shadow) = resolve_outline_css(body);
                result.push((source, f64_val, outline, shadow));
            }
        }
    }
}

fn apply_outline(
    mut el: RawHtmlEl<web_sys::HtmlElement>,
    value: &IrExpr,
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
) -> RawHtmlEl<web_sys::HtmlElement> {
    match value {
        IrExpr::CellRead(cell) => {
            if let Some(node) = find_node_for_cell(program, *cell) {
                match node {
                    IrNode::While { source, arms, .. } | IrNode::When { source, arms, .. } => {
                        // Collect all (source_cell, pattern_f64, outline, shadow) entries,
                        // recursively following nested WHILEs.
                        let mut all_css = Vec::new();
                        collect_outline_arm_css(program, instance, arms, *source, &mut all_css);

                        // Collect unique source cells to watch.
                        let mut source_cells: Vec<CellId> = all_css.iter().map(|(s, _, _, _)| *s).collect();
                        source_cells.sort_by_key(|c| c.0);
                        source_cells.dedup();

                        // Set initial outline from current cell values.
                        let initial_css = resolve_active_outline(&all_css, &instance.cell_store);
                        if let Some((outline, shadow)) = initial_css {
                            if !outline.is_empty() { el = el.style("outline", &outline); }
                            if !shadow.is_empty() { el = el.style("box-shadow", &shadow); }
                        }

                        // Watch all source cells and update outline reactively.
                        let store = instance.cell_store.clone();
                        let source_cells_clone = source_cells.clone();
                        el = el.after_insert(move |element: web_sys::HtmlElement| {
                            for source_cell in &source_cells_clone {
                                let all_css = all_css.clone();
                                let store2 = store.clone();
                                let el2 = element.clone();
                                let handle = Task::start_droppable(
                                    store.get_cell_signal(source_cell.0)
                                        .for_each_sync(move |_val| {
                                            if let Some((outline, shadow)) = resolve_active_outline(&all_css, &store2) {
                                                let style = el2.style();
                                                let _ = style.set_property("outline", &outline);
                                                let _ = style.set_property("box-shadow", &shadow);
                                            }
                                        })
                                );
                                std::mem::forget(handle);
                            }
                        });
                    }
                    IrNode::Derived { expr, .. } => {
                        let (outline, shadow) = resolve_outline_css(expr);
                        if !outline.is_empty() { el = el.style("outline", &outline); }
                        if !shadow.is_empty() { el = el.style("box-shadow", &shadow); }
                    }
                    _ => {}
                }
            }
        }
        other => {
            let (outline, shadow) = resolve_outline_css(other);
            if !outline.is_empty() { el = el.style("outline", &outline); }
            if !shadow.is_empty() { el = el.style("box-shadow", &shadow); }
        }
    }
    el
}

/// Given all collected outline arm CSS entries and current cell values,
/// determine which outline is currently active. Priority: first matching entry wins.
fn resolve_active_outline(
    all_css: &[(CellId, f64, String, String)],
    store: &CellStore,
) -> Option<(String, String)> {
    // Walk entries in order — the FIRST one whose source cell matches wins.
    // This preserves the priority: outer WHILE arms checked before inner ones.
    for (source, pat_val, outline, shadow) in all_css {
        let current = store.get_cell_value(source.0);
        if current == *pat_val {
            return Some((outline.clone(), shadow.clone()));
        }
    }
    // Default: no outline
    Some(("none".to_string(), "none".to_string()))
}

/// Apply CSS transform properties.
fn apply_transform(
    mut el: RawHtmlEl<web_sys::HtmlElement>,
    value: &IrExpr,
    program: &IrProgram,
) -> RawHtmlEl<web_sys::HtmlElement> {
    let reconstructed;
    let fields: &[(String, IrExpr)] = match value {
        IrExpr::ObjectConstruct(fields) => fields,
        IrExpr::CellRead(cell) => {
            reconstructed = reconstruct_object_fields(program, *cell);
            &reconstructed
        }
        _ => return el,
    };
    let mut transforms = Vec::new();
    for (name, val) in fields {
        match name.as_str() {
            "rotate" => {
                if let IrExpr::Constant(IrValue::Number(n)) = val {
                    transforms.push(format!("rotate({}deg)", n));
                }
            }
            "scale" => {
                if let IrExpr::Constant(IrValue::Number(n)) = val {
                    transforms.push(format!("scale({})", n));
                }
            }
            _ => {}
        }
    }
    if !transforms.is_empty() {
        el = el.style("transform", &transforms.join(" "));
    }
    el
}

/// Full color resolver — handles Oklch with all parameters.
fn resolve_color_full(expr: &IrExpr) -> Option<String> {
    match expr {
        IrExpr::Constant(IrValue::Tag(tag)) => {
            Some(tag.to_lowercase())
        }
        IrExpr::TaggedObject { tag, fields } if tag == "Oklch" => {
            let mut lightness = 0.0;
            let mut chroma = 0.0;
            let mut hue = 0.0;
            let mut alpha = 1.0;
            for (name, value) in fields {
                if let IrExpr::Constant(IrValue::Number(n)) = value {
                    match name.as_str() {
                        "lightness" => lightness = *n,
                        "chroma" => chroma = *n,
                        "hue" => hue = *n,
                        "alpha" => alpha = *n,
                        _ => {}
                    }
                }
            }
            if alpha < 1.0 {
                Some(format!("oklch({} {} {} / {})", lightness, chroma, hue, alpha))
            } else {
                Some(format!("oklch({} {} {})", lightness, chroma, hue))
            }
        }
        _ => None,
    }
}
