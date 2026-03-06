//! DOM bridge — creates Zoon UI elements from the IR program and connects
//! them to the WASM runtime instance.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use zoon::*;

#[wasm_bindgen]
extern "C" {
    #[allow(dead_code)]
    #[wasm_bindgen(js_namespace = console, js_name = log)]
    fn console_log(s: &str);
}

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
        _ => return,
    };

    // Read the actual browser URL path and set the route cell text.
    // This must happen BEFORE call_init() so WHEN/WHILE arms see the correct route.
    // Set f64 to 0.0 so subsequent bumps produce real numbers (not NaN + 1 = NaN).
    let window = web_sys::window().unwrap();
    let path = window
        .location()
        .pathname()
        .unwrap_or_else(|_| "/".to_string());
    instance
        .cell_store
        .set_cell_text(route_cell.0, path.clone());
    instance.cell_store.set_cell_f64(route_cell.0, 0.0);

    // Find all RouterGoTo nodes and set up watchers on their source cells.
    for node in &program.nodes {
        if let IrNode::RouterGoTo { source, .. } = node {
            let source_id = source.0;
            let inst = instance.clone();
            let rc = route_cell;
            let re = route_event;
            // Watch the goto source cell for changes → update route cell + push history.
            let handle =
                Task::start_droppable(inst.cell_store.get_cell_signal(source_id).for_each_sync(
                    move |_val| {
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
                    },
                ));
            std::mem::forget(handle);
        }
    }

    // Set up popstate listener for browser back/forward navigation.
    let inst = instance.clone();
    let cb = wasm_bindgen::closure::Closure::wrap(Box::new(move |_event: web_sys::Event| {
        let window = web_sys::window().unwrap();
        let path = window
            .location()
            .pathname()
            .unwrap_or_else(|_| "/".to_string());
        inst.cell_store.set_cell_text(route_cell.0, path);
        inst.set_cell_value(
            route_cell.0,
            inst.cell_store.get_cell_value(route_cell.0) + 1.0,
        );
        let _ = inst.fire_event(route_event.0);
    }) as Box<dyn FnMut(web_sys::Event)>);
    let window = web_sys::window().unwrap();
    let _ = window.add_event_listener_with_callback("popstate", cb.as_ref().unchecked_ref());
    cb.forget();
}

// ---------------------------------------------------------------------------
// Element builders
// ---------------------------------------------------------------------------

fn build_cell_element(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    cell: CellId,
) -> RawElOrText {
    build_element(program, instance, &BuildContext::Global, cell)
}

fn build_element(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    ctx: &BuildContext,
    cell: CellId,
) -> RawElOrText {
    if let Some(node) = find_node_for_cell(program, cell) {
        match node {
            IrNode::Element {
                kind,
                links,
                hovered_cell,
                ..
            } => match ctx {
                BuildContext::Item(item_ctx) => {
                    build_item_element_node(program, instance, item_ctx, kind, links, *hovered_cell)
                }
                BuildContext::Global => {
                    build_element_node(program, instance, kind, links, *hovered_cell)
                }
            },
            IrNode::Derived { expr, .. } => match expr {
                IrExpr::CellRead(source) => build_element(program, instance, ctx, *source),
                IrExpr::Constant(IrValue::Text(t)) => zoon::Text::new(t.clone()).unify(),
                IrExpr::Constant(IrValue::Tag(t)) if t == "NoElement" => El::new().unify(),
                IrExpr::Constant(IrValue::Tag(t)) => zoon::Text::new(t.clone()).unify(),
                IrExpr::Constant(IrValue::Number(n)) => zoon::Text::new(format_number(*n)).unify(),
                IrExpr::TextConcat(segments) => match ctx {
                    BuildContext::Item(item_ctx) => {
                        build_item_text_from_segments(instance, item_ctx, segments)
                    }
                    BuildContext::Global => build_text_from_segments(instance, segments),
                },
                _ => build_reactive_text_ctx(instance, ctx, cell),
            },
            IrNode::TextInterpolation { parts, .. } => match ctx {
                BuildContext::Item(item_ctx) => {
                    build_item_text_from_segments(instance, item_ctx, parts)
                }
                BuildContext::Global => build_text_interpolation(instance, parts),
            },
            IrNode::PipeThrough { source, .. } => build_element(program, instance, ctx, *source),
            IrNode::While { source, arms, .. } => {
                let has_el = has_element_arms(program, arms);
                if has_el {
                    build_conditional_element(program, instance, ctx, *source, arms)
                } else {
                    build_reactive_text_ctx(instance, ctx, cell)
                }
            }
            IrNode::When { source, arms, .. } => {
                if has_element_arms(program, arms) {
                    build_conditional_element(program, instance, ctx, *source, arms)
                } else {
                    build_reactive_text_ctx(instance, ctx, cell)
                }
            }
            IrNode::Latest { .. }
            | IrNode::MathSum { .. }
            | IrNode::Hold { .. }
            | IrNode::Then { .. }
            | IrNode::ListCount { .. }
            | IrNode::HoldLoop { .. }
            | IrNode::ListIsEmpty { .. }
            | IrNode::ListEvery { .. }
            | IrNode::ListAny { .. }
            | IrNode::TextTrim { .. }
            | IrNode::TextIsNotEmpty { .. }
            | IrNode::TextToNumber { .. }
            | IrNode::MathRound { .. }
            | IrNode::TextStartsWith { .. } => build_reactive_text_ctx(instance, ctx, cell),
            IrNode::ListAppend { source, .. }
            | IrNode::ListClear { source, .. }
            | IrNode::ListRemove { source, .. }
            | IrNode::ListRetain { source, .. }
            | IrNode::RouterGoTo { source, .. } => build_element(program, instance, ctx, *source),
            IrNode::ListMap {
                source,
                item_name,
                item_cell,
                template,
                template_cell_range,
                template_event_range,
                ..
            } => build_list_map(
                program,
                instance,
                cell,
                *source,
                *item_cell,
                item_name,
                template,
                *template_cell_range,
                *template_event_range,
            ),
            IrNode::Document { root } => build_element(program, instance, ctx, *root),
            IrNode::CustomCall { path, .. } => {
                let path_str = path.join("/");
                zoon::Text::new(format!("[{}]", path_str)).unify()
            }
            _ => {
                zoon::Text::new("?").unify()
            }
        }
    } else {
        // No node found for cell — falls back to reactive text
        build_reactive_text_ctx(instance, ctx, cell)
    }
}

/// Build reactive text that routes to per-item or global cell store based on context.
fn build_reactive_text_ctx(
    instance: &Rc<WasmInstance>,
    ctx: &BuildContext,
    cell: CellId,
) -> RawElOrText {
    match ctx {
        BuildContext::Item(item_ctx) if item_ctx.is_template_cell(cell) => {
            build_item_reactive_text(instance, item_ctx, cell)
        }
        _ => {
            build_reactive_text(instance, cell)
        }
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
/// Uses `has_text()` (not `!text.is_empty()`) to distinguish "intentionally
/// empty text" (e.g. Text/empty()) from "no text set" (pure numeric cell).
fn format_cell_value(store: &super::runtime::CellStore, cell_id: u32) -> String {
    if store.has_text(cell_id) {
        store.get_cell_text(cell_id)
    } else {
        format_number(store.get_cell_value(cell_id))
    }
}

/// Build text from TextSegment list.
fn build_text_interpolation(instance: &Rc<WasmInstance>, parts: &[TextSegment]) -> RawElOrText {
    build_text_from_segments(instance, parts)
}

fn build_text_from_segments(instance: &Rc<WasmInstance>, segments: &[TextSegment]) -> RawElOrText {
    // Check if any segments are reactive (CellRead).
    let has_reactive = segments
        .iter()
        .any(|seg| matches!(seg, TextSegment::Expr(IrExpr::CellRead(_))));

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
            seg_desc
                .iter()
                .map(|s| match s {
                    SegDesc::Lit(t) => t.clone(),
                    SegDesc::Cell(id) => format_cell_value(&store, *id),
                })
                .collect::<String>()
        }))
        .unify()
    } else {
        // Multiple reactive cells — combine signals.
        // Use first cell as primary trigger, poll others from store.
        // This is correct because all signals update atomically per event.
        let primary = store.get_cell_signal(cell_ids[0]);
        zoon::Text::with_signal(primary.map(move |_| {
            seg_desc
                .iter()
                .map(|s| match s {
                    SegDesc::Lit(t) => t.clone(),
                    SegDesc::Cell(id) => format_cell_value(&store, *id),
                })
                .collect::<String>()
        }))
        .unify()
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
    // Track which event names are handled directly by the element builder
    // so attach_common_events doesn't duplicate them.
    let mut handled_events: &[&str] = &[];

    // All typed element builders handle hover via .on_hovered_change() directly.
    let raw: RawElOrText = match kind {
        ElementKind::Button { label, style } => build_button(
            program,
            instance,
            &BuildContext::Global,
            label,
            style,
            links,
            hovered_cell,
        ),
        ElementKind::Stripe {
            direction,
            items,
            gap,
            style,
            ..
        } => build_stripe(
            program,
            instance,
            &BuildContext::Global,
            direction,
            *items,
            gap,
            style,
            hovered_cell,
        ),
        ElementKind::TextInput {
            placeholder,
            style,
            focus,
            text_cell,
        } => build_text_input(
            program,
            instance,
            placeholder.as_ref(),
            style,
            links,
            *focus,
            hovered_cell,
            *text_cell,
        ),
        ElementKind::Checkbox {
            checked,
            style,
            icon,
        } => {
            handled_events = &["click"];
            build_checkbox(
                program,
                instance,
                checked.as_ref(),
                style,
                links,
                icon.as_ref(),
                hovered_cell,
            )
        }
        ElementKind::Container { child, style } => build_container(
            program,
            instance,
            &BuildContext::Global,
            *child,
            style,
            hovered_cell,
        ),
        ElementKind::Label { label, style } => build_label(
            program,
            instance,
            &BuildContext::Global,
            label,
            style,
            links,
            hovered_cell,
        ),
        ElementKind::Stack { layers, style } => build_stack(
            program,
            instance,
            &BuildContext::Global,
            *layers,
            style,
            hovered_cell,
        ),
        ElementKind::Link { url, label, style } => {
            build_link(program, instance, label, url, style, links, hovered_cell)
        }
        ElementKind::Paragraph { content, style } => {
            build_paragraph(program, instance, content, style, hovered_cell)
        }
        ElementKind::Block { child, style } => build_block(
            program,
            instance,
            &BuildContext::Global,
            *child,
            style,
            hovered_cell,
        ),
        ElementKind::Text { label, style } => build_text_element(
            program,
            instance,
            &BuildContext::Global,
            label,
            style,
            hovered_cell,
        ),
        ElementKind::Slider {
            style,
            value_cell,
            min,
            max,
            step,
        } => build_slider(
            program,
            instance,
            style,
            links,
            *value_cell,
            *min,
            *max,
            *step,
            hovered_cell,
        ),
        ElementKind::Select {
            style,
            options,
            selected,
        } => build_select(
            program,
            instance,
            style,
            links,
            options,
            selected.as_ref(),
            hovered_cell,
        ),
        ElementKind::Svg { style, children } => build_svg(
            program,
            instance,
            &BuildContext::Global,
            style,
            *children,
            links,
            hovered_cell,
        ),
        ElementKind::SvgCircle { cx, cy, r, style } => build_svg_circle(
            program,
            instance,
            &BuildContext::Global,
            cx,
            cy,
            r,
            style,
        ),
    };
    // Attach remaining event handlers (blur, focus, click, double_click).
    // Hover is handled by typed builders via .on_hovered_change().
    if handled_events.is_empty() {
        attach_common_events(raw, instance, links, None)
    } else {
        let filtered: Vec<_> = links
            .iter()
            .filter(|(name, _)| !handled_events.contains(&name.as_str()))
            .cloned()
            .collect();
        attach_common_events(raw, instance, &filtered, None)
    }
}

/// Attach common event handlers (hovered, blur, focus, click, double_click) to an element.
/// If no common events are needed, returns the element unchanged.
/// Events are attached directly to the element (no wrapper div) to avoid
/// Chrome's buggy mouse events on `display: contents` elements.
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
    let double_click_event = links
        .iter()
        .find(|(n, _)| n == "double_click")
        .map(|(_, e)| *e);

    if !has_hover
        && blur_event.is_none()
        && focus_event.is_none()
        && click_event.is_none()
        && double_click_event.is_none()
    {
        return el;
    }

    // Extract the HTML element for direct event attachment (text nodes can't have events).
    let mut html_el = match el {
        RawElOrText::RawHtmlEl(html_el) => html_el,
        other => return other,
    };

    // Hover: mouseenter → set cell to True (1.0), mouseleave → False (0.0).
    if let Some(cell) = hovered_cell {
        let inst = instance.clone();
        html_el = html_el.event_handler(move |_: events::MouseEnter| {
            inst.set_cell_value(cell.0, 1.0);
        });
        let inst = instance.clone();
        html_el = html_el.event_handler(move |_: events::MouseLeave| {
            inst.set_cell_value(cell.0, 0.0);
        });
    }

    // Blur event.
    if let Some(event_id) = blur_event {
        let inst = instance.clone();
        html_el = html_el.event_handler(move |_: events::Blur| {
            let _ = inst.fire_event(event_id.0);
        });
    }

    // Focus event.
    if let Some(event_id) = focus_event {
        let inst = instance.clone();
        html_el = html_el.event_handler(move |_: events::Focus| {
            let _ = inst.fire_event(event_id.0);
        });
    }

    // Click event.
    if let Some(event_id) = click_event {
        let inst = instance.clone();
        html_el = html_el.event_handler(move |_: events::Click| {
            let _ = inst.fire_event(event_id.0);
        });
    }

    // Double-click event.
    if let Some(event_id) = double_click_event {
        let inst = instance.clone();
        html_el = html_el.event_handler(move |_: events::DoubleClick| {
            let _ = inst.fire_event(event_id.0);
        });
    }

    html_el.into_raw_unchecked()
}

/// Resolve a label expression to TextConcat segments (following CellRead chains).
fn resolve_label_segments<'a>(
    program: &'a IrProgram,
    label: &'a IrExpr,
) -> Option<&'a [TextSegment]> {
    match label {
        IrExpr::TextConcat(segs) => Some(segs),
        IrExpr::CellRead(cell) => {
            if let Some(IrNode::Derived {
                expr: IrExpr::TextConcat(segs),
                ..
            }) = find_node_for_cell(program, *cell)
            {
                Some(segs)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Build a label child element — reactive if it contains CellRead segments, static otherwise.
/// Also handles the case where the label is an Element node (e.g. Scene/Element/text).
fn build_label_child(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    ctx: &BuildContext,
    label: &IrExpr,
) -> RawElOrText {
    // Check if label is a CellRead pointing to an Element node (e.g. Scene/Element/text).
    // In that case, build the element directly instead of trying to extract text.
    if let IrExpr::CellRead(cell) = label {
        if matches!(find_node_for_cell(program, *cell), Some(IrNode::Element { .. })) {
            let el = build_element(program, instance, ctx, *cell);
            // pointer-events:none so clicks pass through to the button div.
            return match el {
                RawElOrText::RawHtmlEl(el) => {
                    RawElOrText::RawHtmlEl(el.style("pointer-events", "none"))
                }
                other => other,
            };
        }
        // For per-item labels: if label CellRead points to a template cell,
        // use per-item reactive text that reads from ICS. This handles
        // labels computed by WHILE/WHEN arms with TEXT interpolation
        // (e.g., TEXT { {person.surname}, {person.name} }) whose text
        // was set during init_item.
        if let BuildContext::Item(item_ctx) = ctx {
            if item_ctx.is_template_cell(*cell) {
                let el = build_item_reactive_text(instance, item_ctx, *cell);
                return match el {
                    RawElOrText::RawHtmlEl(el) => {
                        RawElOrText::RawHtmlEl(el.style("pointer-events", "none"))
                    }
                    other => other,
                };
            }
        }
    }
    let el = if let Some(segs) = resolve_label_segments(program, label) {
        if segs
            .iter()
            .any(|s| matches!(s, TextSegment::Expr(IrExpr::CellRead(_))))
        {
            if let BuildContext::Item(item_ctx) = ctx {
                return build_item_text_from_segments(instance, item_ctx, segs);
            }
            return build_text_from_segments(instance, segs);
        }
        zoon::Text::new(resolve_static_text(program, label)).unify()
    } else {
        zoon::Text::new(resolve_static_text(program, label)).unify()
    };
    // pointer-events:none on label so clicks pass through to the button div.
    // Without this, dominator's event handler doesn't fire for clicks that
    // land on label children (same issue as checkbox icons).
    match el {
        RawElOrText::RawHtmlEl(el) => RawElOrText::RawHtmlEl(el.style("pointer-events", "none")),
        other => other,
    }
}

/// Build a Button element (works for both global and per-item contexts).
fn build_button(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    ctx: &BuildContext,
    label: &IrExpr,
    style: &IrExpr,
    links: &[(String, EventId)],
    hovered_cell: Option<CellId>,
) -> RawElOrText {
    let press_event = links
        .iter()
        .find(|(name, _)| name == "press")
        .map(|(_, eid)| *eid);

    let label_child = build_label_child(program, instance, ctx, label);
    let inst_press = instance.clone();
    let is_template_event = press_event
        .map(|e| ctx.is_template_event(e))
        .unwrap_or(false);
    let item_idx = ctx.item_ctx().map(|c| c.item_idx);
    let mut btn = Button::new().label(label_child).on_press(move || {
        if let Some(event_id) = press_event {
            if is_template_event {
                let _ = inst_press.call_on_item_event(item_idx.unwrap(), event_id.0);
            } else {
                let _ = inst_press.fire_event(event_id.0);
            }
        }
    });
    btn = apply_typed_styles(btn, style, program, false);
    if let Some(cell) = hovered_cell {
        btn = apply_hover(btn, instance, ctx.item_ctx(), cell);
    }
    btn.update_raw_el(|raw_el| {
        let raw_el = apply_raw_css(raw_el, style, program, instance, ctx.item_ctx(), false);
        apply_physical_css(raw_el, style, program, instance, ctx.item_ctx())
    })
    .into_raw_unchecked()
}

/// A child slot for Stripe elements — either a static element or a reactive signal.
enum ChildSlot {
    Static(RawElOrText),
    Signal(Box<dyn Signal<Item = Option<RawElOrText>> + Unpin>),
}

/// Follow CellRead/Derived/PipeThrough chains to find if this cell resolves to a
/// WHEN/WHILE node with element arms. Returns (source, arms) if found.
fn resolve_to_conditional(
    program: &IrProgram,
    cell: CellId,
) -> Option<(CellId, &[(IrPattern, IrExpr)])> {
    match find_node_for_cell(program, cell) {
        Some(IrNode::When { source, arms, .. }) | Some(IrNode::While { source, arms, .. }) => {
            if has_element_arms(program, arms) {
                Some((*source, arms))
            } else {
                None
            }
        }
        Some(IrNode::Derived {
            expr: IrExpr::CellRead(inner),
            ..
        }) => resolve_to_conditional(program, *inner),
        Some(IrNode::PipeThrough { source, .. }) => resolve_to_conditional(program, *source),
        _ => None,
    }
}

/// Build a Stripe (row/column) element (works for both global and per-item contexts).
fn build_stripe(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    ctx: &BuildContext,
    direction: &IrExpr,
    items_cell: CellId,
    gap: &IrExpr,
    style: &IrExpr,
    hovered_cell: Option<CellId>,
) -> RawElOrText {
    let children = collect_stripe_children(program, instance, ctx, items_cell);
    let is_column = match direction {
        IrExpr::Constant(IrValue::Tag(t)) => t == "Column",
        _ => true,
    };

    let is_row = !is_column;
    let mut stripe = if is_column {
        Stripe::new()
    } else {
        Stripe::new().direction(Direction::Row)
    };
    stripe = apply_typed_styles(stripe, style, program, is_row);
    if let IrExpr::Constant(IrValue::Number(n)) = gap {
        if *n > 0.0 {
            stripe = stripe.s(Gap::both(*n as u32));
        }
    }
    if let Some(cell) = hovered_cell {
        stripe = apply_hover(stripe, instance, ctx.item_ctx(), cell);
    }
    stripe
        .update_raw_el(|raw_el| {
            let mut raw_el = raw_el;
            for slot in children {
                match slot {
                    ChildSlot::Static(el) => {
                        raw_el = raw_el.child(el);
                    }
                    ChildSlot::Signal(sig) => {
                        raw_el = raw_el.child_signal(sig);
                    }
                }
            }
            let raw_el = apply_raw_css(raw_el, style, program, instance, ctx.item_ctx(), is_row);
            apply_physical_css(raw_el, style, program, instance, ctx.item_ctx())
        })
        .into_raw_unchecked()
}

/// Collect children for a stripe element (works for both global and per-item contexts).
fn collect_stripe_children(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    ctx: &BuildContext,
    items_cell: CellId,
) -> Vec<ChildSlot> {
    match find_node_for_cell(program, items_cell) {
        Some(IrNode::Derived { expr, .. }) => {
            match expr {
                IrExpr::ListConstruct(items) => {
                    let mut children = Vec::new();
                    for item in items {
                        match item {
                            IrExpr::CellRead(child_cell) => {
                                let cond = resolve_to_conditional(program, *child_cell);
                                if let Some((source, arms)) = cond
                                {
                                    if let (BuildContext::Item(item_ctx), true) =
                                        (ctx, has_no_element_arm(program, arms))
                                    {
                                        // Per-item + NoElement (e.g., hover X button):
                                        // Pre-build with visibility toggle to keep elements in DOM.
                                        // Using visibility:hidden prevents pointer-event interference
                                        // while avoiding DOM insertion/removal that causes hover
                                        // oscillation from child_signal.
                                        for el in build_item_visibility_children(
                                            program, instance, item_ctx, source, arms,
                                        ) {
                                            children.push(ChildSlot::Static(el));
                                        }
                                    } else {
                                        // All other conditionals: use child_signal.
                                        children.push(ChildSlot::Signal(build_conditional_signal(
                                            program, instance, ctx, source, arms,
                                        )));
                                    }
                                } else {
                                    children.push(ChildSlot::Static(build_element(
                                        program,
                                        instance,
                                        ctx,
                                        *child_cell,
                                    )));
                                }
                            }
                            IrExpr::Constant(IrValue::Text(t)) => {
                                children
                                    .push(ChildSlot::Static(zoon::Text::new(t.clone()).unify()));
                            }
                            IrExpr::Constant(IrValue::Tag(t)) => {
                                if t != "NoElement" {
                                    children.push(ChildSlot::Static(
                                        zoon::Text::new(t.clone()).unify(),
                                    ));
                                }
                            }
                            IrExpr::TextConcat(segments) => match ctx {
                                BuildContext::Item(item_ctx) => children.push(ChildSlot::Static(
                                    build_item_text_from_segments(instance, item_ctx, segments),
                                )),
                                BuildContext::Global => children.push(ChildSlot::Static(
                                    build_text_from_segments(instance, segments),
                                )),
                            },
                            _other => {}
                        }
                    }
                    children
                }
                IrExpr::CellRead(source) => {
                    collect_stripe_children(program, instance, ctx, *source)
                }
                _ => {
                    Vec::new()
                }
            }
        }
        Some(IrNode::While { source, arms, .. })
        | Some(IrNode::When { source, arms, .. }) => {
            // Conditional items (WHILE/WHEN wrapping LIST branches):
            // Use ChildSlot::Signal so the conditional content participates
            // directly in the parent stripe's flex layout without an extra
            // wrapper div that would break Row direction.
            vec![ChildSlot::Signal(build_conditional_signal(
                program, instance, ctx, *source, arms,
            ))]
        }
        Some(_) => {
            vec![ChildSlot::Static(build_element(
                program, instance, ctx, items_cell,
            ))]
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
    hovered_cell: Option<CellId>,
    boon_text_cell: Option<CellId>,
) -> RawElOrText {
    // Find events from links.
    let key_down_event = links
        .iter()
        .find(|(name, _)| name == "key_down")
        .map(|(_, eid)| *eid);
    let change_event = links
        .iter()
        .find(|(name, _)| name == "change")
        .map(|(_, eid)| *eid);

    // Find data cells for event payloads.
    let key_data_cell = find_data_cell_for_event(program, links, "key_down", "key");
    let change_text_cell = find_data_cell_for_event(program, links, "change", "text");
    let text_cell = find_text_property_cell(program, links);
    let inherit_text_color = !style_defines_font_color(style, program);

    let mut ti = TextInput::new();
    ti = apply_typed_styles(ti, style, program, false);
    if let Some(cell) = hovered_cell {
        ti = apply_hover(ti, instance, None, cell);
    }
    // Apply placeholder via Zoon's typed Placeholder API.
    let ph_el = build_placeholder(placeholder, program);
    ti.placeholder(ph_el)
        .update_raw_el(|raw_el| {
            let mut raw_el = raw_el
                .style("box-sizing", "border-box")
                .style("outline", "none");

            if inherit_text_color {
                raw_el = raw_el.style("color", "inherit");
            }

            if focus {
                raw_el = raw_el.attr("autofocus", "");
                raw_el = raw_el.after_insert(|el| {
                    let _ = el.focus();
                });
            }

            // Keep DOM value synchronized with `element.text` cell state so
            // reruns and persistence restores show the stored draft text.
            if let Some(cell_id) = text_cell {
                let inst = instance.clone();
                raw_el = raw_el.after_insert(move |input_el: web_sys::HtmlInputElement| {
                    let store = inst.cell_store.clone();

                    // Apply current value immediately on mount.
                    let initial = store.get_cell_text(cell_id);
                    if input_el.value() != initial {
                        input_el.set_value(&initial);
                    }

                    // Reactively apply future updates from the text cell.
                    let signal_store = store.clone();
                    let read_store = store.clone();
                    let input_for_signal = input_el.clone();
                    let handle =
                        Task::start_droppable(signal_store.get_cell_signal(cell_id).for_each_sync(
                            move |_| {
                                let text = read_store.get_cell_text(cell_id);
                                if input_for_signal.value() != text {
                                    input_for_signal.set_value(&text);
                                }
                            },
                        ));
                    std::mem::forget(handle);
                });
            }

            // Reactively bind the Boon `text:` argument cell to the DOM value.
            // This is separate from the LINK `.text` cell above: the LINK cell
            // tracks user-typed text, while boon_text_cell carries the Boon-computed
            // value (e.g., a conversion result) that should control the display.
            // Uses get_cell_text (not format_cell_value) so empty text = empty input
            // (showing placeholder) rather than formatting f64 counter as "0".
            if let Some(btc) = boon_text_cell {
                let btc_id = btc.0;
                let inst = instance.clone();
                raw_el = raw_el.after_insert(move |input_el: web_sys::HtmlInputElement| {
                    let store = inst.cell_store.clone();
                    let read_store = store.clone();
                    let handle =
                        Task::start_droppable(store.get_cell_signal(btc_id).for_each_sync(
                            move |_| {
                                let text = read_store.get_cell_text(btc_id);
                                if input_el.value() != text {
                                    input_el.set_value(&text);
                                }
                            },
                        ));
                    std::mem::forget(handle);
                });
            }

            raw_el = apply_raw_css(raw_el, style, program, instance, None, false);
            raw_el = apply_physical_css(raw_el, style, program, instance, None);

            // Set up keydown event listener.
            let raw_el = if let Some(event_id) = key_down_event {
                let inst = instance.clone();
                raw_el.event_handler(move |event: events::KeyDown| {
                    let key = event.key();
                    let tag_value = match key.as_str() {
                        "Enter" => inst.program_tag_index("Enter"),
                        "Escape" => inst.program_tag_index("Escape"),
                        _ => 0.0,
                    };
                    if let Some(target) = event.target() {
                        if let Ok(input) = target.dyn_into::<web_sys::HtmlInputElement>() {
                            let input_text = input.value();
                            if let Some(cell_id) = change_text_cell {
                                inst.cell_store.set_cell_text(cell_id, input_text.clone());
                            }
                            if let Some(cell_id) = text_cell {
                                inst.cell_store.set_cell_text(cell_id, input_text.clone());
                            }
                            if key == "Enter" {
                                input.set_value("");
                            }
                        }
                    }
                    if let Some(cell_id) = key_data_cell {
                        inst.set_cell_value(cell_id, tag_value);
                    }
                    let _ = inst.fire_event(event_id.0);
                })
            } else {
                raw_el
            };

            // Set up input change event listener.
            if let Some(event_id) = change_event {
                let inst = instance.clone();
                raw_el.event_handler(move |event: events::Input| {
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
                raw_el
            }
        })
        .into_raw_unchecked()
}

/// Build a slider (`<input type="range">`) element with change event.
fn build_slider(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    style: &IrExpr,
    links: &[(String, EventId)],
    value_cell: Option<CellId>,
    min: f64,
    max: f64,
    step: f64,
    hovered_cell: Option<CellId>,
) -> RawElOrText {
    let change_event = links
        .iter()
        .find(|(name, _)| name == "change")
        .map(|(_, eid)| *eid);
    let change_value_cell = find_data_cell_for_event(program, links, "change", "value");

    let mut raw_el = RawHtmlEl::new("input")
        .attr("type", "range")
        .attr("min", &min.to_string())
        .attr("max", &max.to_string())
        .attr("step", &step.to_string());

    // Set initial value from the value_cell if available.
    if let Some(vc) = value_cell {
        let inst = instance.clone();
        raw_el = raw_el.after_insert(move |el: web_sys::HtmlElement| {
            let input_el: web_sys::HtmlInputElement = el.unchecked_into();
            let store = inst.cell_store.clone();
            let val = store.get_cell_value(vc.0);
            if val.is_finite() {
                input_el.set_value(&val.to_string());
            }
            // Reactively update from the cell.
            let read_store = store.clone();
            let input_for_signal = input_el.clone();
            let handle =
                Task::start_droppable(store.get_cell_signal(vc.0).for_each_sync(move |_| {
                    let v = read_store.get_cell_value(vc.0);
                    if v.is_finite() {
                        let new_val = v.to_string();
                        if input_for_signal.value() != new_val {
                            input_for_signal.set_value(&new_val);
                        }
                    }
                }));
            std::mem::forget(handle);
        });
    }

    // Apply style (width etc.).
    raw_el = apply_raw_css(raw_el, style, program, instance, None, false);

    // Handle hover.
    if let Some(cell) = hovered_cell {
        let inst = instance.clone();
        raw_el = raw_el.event_handler(move |_: events::MouseEnter| {
            inst.set_cell_value(cell.0, 1.0);
        });
        let inst = instance.clone();
        raw_el = raw_el.event_handler(move |_: events::MouseLeave| {
            inst.set_cell_value(cell.0, 0.0);
        });
    }

    // Handle change event — fires on input to give live updates.
    if let Some(event_id) = change_event {
        let inst = instance.clone();
        raw_el = raw_el.event_handler(move |event: events::Input| {
            if let Some(target) = event.target() {
                if let Ok(input) = target.dyn_into::<web_sys::HtmlInputElement>() {
                    let val: f64 = input.value().parse().unwrap_or(0.0);
                    if let Some(cell_id) = change_value_cell {
                        inst.set_cell_value(cell_id, val);
                    }
                }
            }
            let _ = inst.fire_event(event_id.0);
        });
    }

    raw_el.into_raw_unchecked()
}

/// Build a select (`<select>`) element with static options and change event.
fn build_select(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    style: &IrExpr,
    links: &[(String, EventId)],
    options: &[(String, String)],
    selected: Option<&IrExpr>,
    hovered_cell: Option<CellId>,
) -> RawElOrText {
    let change_event = links
        .iter()
        .find(|(name, _)| name == "change")
        .map(|(_, eid)| *eid);
    let change_value_cell = find_data_cell_for_event(program, links, "change", "value");

    let initial_selected = selected
        .and_then(|s| match s {
            IrExpr::Constant(IrValue::Text(t)) => Some(t.clone()),
            _ => None,
        })
        .unwrap_or_default();

    let mut raw_el = RawHtmlEl::new("select");

    // Build option children.
    for (value, label) in options {
        let opt = RawHtmlEl::new("option")
            .attr("value", value)
            .child(zoon::Text::new(label.clone()));
        if *value == initial_selected {
            raw_el = raw_el.child(opt.attr("selected", ""));
        } else {
            raw_el = raw_el.child(opt);
        }
    }

    // Apply style.
    raw_el = apply_raw_css(raw_el, style, program, instance, None, false);

    // Handle hover.
    if let Some(cell) = hovered_cell {
        let inst = instance.clone();
        raw_el = raw_el.event_handler(move |_: events::MouseEnter| {
            inst.set_cell_value(cell.0, 1.0);
        });
        let inst = instance.clone();
        raw_el = raw_el.event_handler(move |_: events::MouseLeave| {
            inst.set_cell_value(cell.0, 0.0);
        });
    }

    // Handle change event — dispatches `input` event to match Element/select convention.
    if let Some(event_id) = change_event {
        let inst = instance.clone();
        raw_el = raw_el.event_handler(move |event: events::Input| {
            if let Some(target) = event.target() {
                // HtmlSelectElement feature not enabled in web_sys, so get value via js_sys.
                let value_key = wasm_bindgen::JsValue::from_str("value");
                if let Ok(val) = js_sys::Reflect::get(&target, &value_key) {
                    let selected_value = val.as_string().unwrap_or_default();
                    if let Some(cell_id) = change_value_cell {
                        inst.cell_store.set_cell_text(cell_id, selected_value);
                    }
                }
            }
            let _ = inst.fire_event(event_id.0);
        });
    }

    raw_el.into_raw_unchecked()
}

/// Build an SVG container element with click event carrying x/y coordinates.
fn build_svg(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    ctx: &BuildContext,
    style: &IrExpr,
    children_cell: CellId,
    links: &[(String, EventId)],
    hovered_cell: Option<CellId>,
) -> RawElOrText {
    let click_event = links
        .iter()
        .find(|(name, _)| name == "click")
        .map(|(_, eid)| *eid);
    let click_x_cell = find_data_cell_for_event(program, links, "click", "x");
    let click_y_cell = find_data_cell_for_event(program, links, "click", "y");

    // Extract width/height/background from style.
    let (width, height, background) = extract_svg_style(style, program);

    let mut svg = RawSvgEl::new("svg")
        .attr("width", &width.to_string())
        .attr("height", &height.to_string())
        .style("overflow", "visible");

    if !background.is_empty() {
        svg = svg.style("background", &background);
    }

    // Add SVG children (circles etc.).
    let children = collect_svg_children(program, instance, ctx, children_cell);
    for child in children {
        svg = svg.child(child);
    }

    // Handle hover.
    if let Some(cell) = hovered_cell {
        let inst = instance.clone();
        svg = svg.event_handler(move |_: events::MouseEnter| {
            inst.set_cell_value(cell.0, 1.0);
        });
        let inst = instance.clone();
        svg = svg.event_handler(move |_: events::MouseLeave| {
            inst.set_cell_value(cell.0, 0.0);
        });
    }

    // Handle click event with coordinates.
    if let Some(event_id) = click_event {
        let inst = instance.clone();
        svg = svg.event_handler(move |event: events::Click| {
            let x = f64::from(event.offset_x());
            let y = f64::from(event.offset_y());
            if let Some(cell_id) = click_x_cell {
                inst.set_cell_value(cell_id, x);
            }
            if let Some(cell_id) = click_y_cell {
                inst.set_cell_value(cell_id, y);
            }
            let _ = inst.fire_event(event_id.0);
        });
    }

    svg.into_raw_unchecked()
}

/// Extract width, height, and background from an SVG style expression.
fn extract_svg_style(style: &IrExpr, program: &IrProgram) -> (f64, f64, String) {
    let reconstructed;
    let fields: &[(String, IrExpr)] = match style {
        IrExpr::ObjectConstruct(fields) => fields,
        IrExpr::CellRead(cell) => {
            reconstructed = reconstruct_object_fields(program, *cell);
            &reconstructed
        }
        _ => return (300.0, 150.0, String::new()),
    };
    let mut width = 300.0;
    let mut height = 150.0;
    let mut background = String::new();
    for (name, value) in fields {
        match name.as_str() {
            "width" => {
                if let IrExpr::Constant(IrValue::Number(n)) = value {
                    width = *n;
                }
            }
            "height" => {
                if let IrExpr::Constant(IrValue::Number(n)) = value {
                    height = *n;
                }
            }
            "background" => {
                if let IrExpr::Constant(IrValue::Text(t)) = value {
                    background = t.clone();
                }
            }
            _ => {}
        }
    }
    (width, height, background)
}

/// Collect SVG child elements from a children cell.
fn collect_svg_children(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    ctx: &BuildContext,
    children_cell: CellId,
) -> Vec<RawElOrText> {
    match find_node_for_cell(program, children_cell) {
        Some(IrNode::Derived { expr, .. }) => match expr {
            IrExpr::ListConstruct(items) => {
                let mut children = Vec::new();
                for item in items {
                    if let IrExpr::CellRead(child_cell) = item {
                        children.push(build_element(program, instance, ctx, *child_cell));
                    }
                }
                children
            }
            IrExpr::CellRead(source) => collect_svg_children(program, instance, ctx, *source),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// Build an SVG circle element.
fn build_svg_circle(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    ctx: &BuildContext,
    cx: &IrExpr,
    cy: &IrExpr,
    r: &IrExpr,
    style: &IrExpr,
) -> RawElOrText {
    let cx_val = resolve_number_expr(cx, program, instance, ctx);
    let cy_val = resolve_number_expr(cy, program, instance, ctx);
    let r_val = resolve_number_expr(r, program, instance, ctx);

    let (fill, stroke, stroke_width) = extract_svg_circle_style(style, program);

    let mut circle = RawSvgEl::new("circle")
        .attr("fill", &fill)
        .attr("stroke", &stroke)
        .attr("stroke-width", &stroke_width.to_string());

    // Apply cx, cy, r — either static or reactive.
    match cx_val {
        NumberOrSignal::Static(v) => {
            circle = circle.attr("cx", &v.to_string());
        }
        NumberOrSignal::Signal(cell_id) => {
            let store = instance.cell_store.clone();
            circle = circle.attr_signal(
                "cx",
                store
                    .get_cell_signal(cell_id)
                    .map(move |_| store.get_cell_value(cell_id).to_string()),
            );
        }
    }
    match cy_val {
        NumberOrSignal::Static(v) => {
            circle = circle.attr("cy", &v.to_string());
        }
        NumberOrSignal::Signal(cell_id) => {
            let store = instance.cell_store.clone();
            circle = circle.attr_signal(
                "cy",
                store
                    .get_cell_signal(cell_id)
                    .map(move |_| store.get_cell_value(cell_id).to_string()),
            );
        }
    }
    match r_val {
        NumberOrSignal::Static(v) => {
            circle = circle.attr("r", &v.to_string());
        }
        NumberOrSignal::Signal(cell_id) => {
            let store = instance.cell_store.clone();
            circle = circle.attr_signal(
                "r",
                store
                    .get_cell_signal(cell_id)
                    .map(move |_| store.get_cell_value(cell_id).to_string()),
            );
        }
    }

    circle.into_raw_unchecked()
}

/// Either a static number or a cell ID for a reactive signal.
enum NumberOrSignal {
    Static(f64),
    Signal(u32),
}

/// Resolve a number expression to either a static value or a cell signal source.
fn resolve_number_expr(
    expr: &IrExpr,
    _program: &IrProgram,
    _instance: &Rc<WasmInstance>,
    _ctx: &BuildContext,
) -> NumberOrSignal {
    match expr {
        IrExpr::Constant(IrValue::Number(n)) => NumberOrSignal::Static(*n),
        IrExpr::CellRead(cell) => NumberOrSignal::Signal(cell.0),
        _ => NumberOrSignal::Static(0.0),
    }
}

/// Extract fill, stroke, and stroke_width from an SVG circle style.
fn extract_svg_circle_style(style: &IrExpr, program: &IrProgram) -> (String, String, f64) {
    let reconstructed;
    let fields: &[(String, IrExpr)] = match style {
        IrExpr::ObjectConstruct(fields) => fields,
        IrExpr::CellRead(cell) => {
            reconstructed = reconstruct_object_fields(program, *cell);
            &reconstructed
        }
        _ => return ("blue".into(), "none".into(), 0.0),
    };
    let mut fill = "blue".to_string();
    let mut stroke = "none".to_string();
    let mut stroke_width = 0.0;
    for (name, value) in fields {
        match name.as_str() {
            "fill" => {
                if let IrExpr::Constant(IrValue::Text(t)) = value {
                    fill = t.clone();
                }
            }
            "stroke" => {
                if let IrExpr::Constant(IrValue::Text(t)) = value {
                    stroke = t.clone();
                }
            }
            "stroke_width" => {
                if let IrExpr::Constant(IrValue::Number(n)) = value {
                    stroke_width = *n;
                }
            }
            _ => {}
        }
    }
    (fill, stroke, stroke_width)
}

fn style_defines_font_color(style: &IrExpr, program: &IrProgram) -> bool {
    let reconstructed_style;
    let style_fields: &[(String, IrExpr)] = match style {
        IrExpr::ObjectConstruct(fields) => fields,
        IrExpr::CellRead(cell) => {
            reconstructed_style = reconstruct_object_fields(program, *cell);
            &reconstructed_style
        }
        _ => return false,
    };

    for (name, value) in style_fields {
        if name != "font" {
            continue;
        }

        let reconstructed_font;
        let font_fields: &[(String, IrExpr)] = match value {
            IrExpr::ObjectConstruct(fields) => fields,
            IrExpr::CellRead(cell) => {
                reconstructed_font = reconstruct_object_fields(program, *cell);
                &reconstructed_font
            }
            _ => continue,
        };

        if font_fields
            .iter()
            .any(|(field_name, _)| field_name == "color")
        {
            return true;
        }
    }

    false
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
    let event_id = links
        .iter()
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
        let template_match = program
            .cells
            .iter()
            .enumerate()
            .find(|(idx, info)| {
                let id = *idx as u32;
                id >= start && id < end && info.name == data_cell_name
            })
            .map(|(idx, _)| idx as u32);
        if template_match.is_some() {
            return template_match;
        }
    }
    program
        .cells
        .iter()
        .enumerate()
        .find(|(_, info)| info.name == data_cell_name)
        .map(|(idx, _)| idx as u32)
}

/// Find the .text property cell for a text input element.
fn find_text_property_cell(program: &IrProgram, links: &[(String, EventId)]) -> Option<u32> {
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
        let template_match = program
            .cells
            .iter()
            .enumerate()
            .find(|(idx, info)| {
                let id = *idx as u32;
                id >= start && id < end && info.name == text_cell_name
            })
            .map(|(idx, _)| idx as u32);
        if template_match.is_some() {
            return template_match;
        }
    }
    program
        .cells
        .iter()
        .enumerate()
        .find(|(_, info)| info.name == text_cell_name)
        .map(|(idx, _)| idx as u32)
}

/// Build a Container element (wraps a single child).
fn build_container(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    ctx: &BuildContext,
    child: CellId,
    style: &IrExpr,
    hovered_cell: Option<CellId>,
) -> RawElOrText {
    let child_el = build_element(program, instance, ctx, child);
    let mut el = El::new();
    el = apply_typed_styles(el, style, program, false);
    if let Some(cell) = hovered_cell {
        el = apply_hover(el, instance, ctx.item_ctx(), cell);
    }
    el.child(child_el)
        .update_raw_el(|raw_el| {
            let raw_el = apply_raw_css(raw_el, style, program, instance, ctx.item_ctx(), false);
            apply_physical_css(raw_el, style, program, instance, ctx.item_ctx())
        })
        .into_raw_unchecked()
}

/// Build a Block element (physical rendering: styled div with a single child).
/// Handles physical CSS properties: material (background-color via Oklch),
/// depth (box-shadow), glow, rounded_corners, padding, width, height.
fn build_block(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    ctx: &BuildContext,
    child: CellId,
    style: &IrExpr,
    hovered_cell: Option<CellId>,
) -> RawElOrText {
    let child_el = build_element(program, instance, ctx, child);
    let mut el = El::new();
    el = apply_typed_styles(el, style, program, false);
    if let Some(cell) = hovered_cell {
        el = apply_hover(el, instance, ctx.item_ctx(), cell);
    }
    el.child(child_el)
        .update_raw_el(|raw_el| {
            let raw_el = apply_raw_css(raw_el, style, program, instance, ctx.item_ctx(), false);
            apply_physical_css(raw_el, style, program, instance, ctx.item_ctx())
        })
        .into_raw_unchecked()
}

/// Build a Text element (physical rendering: styled span with text content).
/// Handles physical CSS properties for text: material (color via Oklch),
/// font size/color, depth, glow, rounded_corners.
fn build_text_element(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    ctx: &BuildContext,
    label: &IrExpr,
    style: &IrExpr,
    hovered_cell: Option<CellId>,
) -> RawElOrText {
    let content: RawElOrText = match label {
        IrExpr::TextConcat(segments) => match ctx {
            BuildContext::Item(item_ctx) => {
                build_item_text_from_segments(instance, item_ctx, segments)
            }
            BuildContext::Global => build_text_from_segments(instance, segments),
        },
        IrExpr::Constant(IrValue::Text(t)) => zoon::Text::new(t.clone()).unify(),
        IrExpr::CellRead(cell) => build_element(program, instance, ctx, *cell),
        _ => {
            let text = eval_static_text(label);
            zoon::Text::new(text).unify()
        }
    };
    let mut lbl = Label::new();
    lbl = apply_typed_styles(lbl, style, program, false);
    if let Some(cell) = hovered_cell {
        lbl = apply_hover(lbl, instance, ctx.item_ctx(), cell);
    }
    lbl.label(content)
        .update_raw_el(|raw_el| {
            let raw_el = apply_raw_css(raw_el, style, program, instance, ctx.item_ctx(), false);
            apply_physical_css(raw_el, style, program, instance, ctx.item_ctx())
        })
        .into_raw_unchecked()
}

/// Build a Label element (displays text).
fn build_label(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    ctx: &BuildContext,
    label: &IrExpr,
    style: &IrExpr,
    _links: &[(String, EventId)],
    hovered_cell: Option<CellId>,
) -> RawElOrText {
    let content: RawElOrText = match label {
        IrExpr::TextConcat(segments) => match ctx {
            BuildContext::Item(item_ctx) => {
                build_item_text_from_segments(instance, item_ctx, segments)
            }
            BuildContext::Global => build_text_from_segments(instance, segments),
        },
        IrExpr::Constant(IrValue::Text(t)) => zoon::Text::new(t.clone()).unify(),
        IrExpr::CellRead(cell) => build_element(program, instance, ctx, *cell),
        _ => {
            let text = eval_static_text(label);
            zoon::Text::new(text).unify()
        }
    };
    let mut lbl = Label::new();
    lbl = apply_typed_styles(lbl, style, program, false);
    if let Some(cell) = hovered_cell {
        lbl = apply_hover(lbl, instance, ctx.item_ctx(), cell);
    }
    lbl.label(content)
        .update_raw_el(|raw_el| {
            let raw_el = apply_raw_css(raw_el, style, program, instance, ctx.item_ctx(), false);
            apply_physical_css(raw_el, style, program, instance, ctx.item_ctx())
        })
        .into_raw_unchecked()
}

/// Build a Stack element (z-axis layering).
fn build_stack(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    ctx: &BuildContext,
    layers_cell: CellId,
    style: &IrExpr,
    hovered_cell: Option<CellId>,
) -> RawElOrText {
    let children = collect_stripe_children(program, instance, ctx, layers_cell);
    let mut stk = Stack::new();
    stk = apply_typed_styles(stk, style, program, false);
    if let Some(cell) = hovered_cell {
        stk = apply_hover(stk, instance, ctx.item_ctx(), cell);
    }
    stk.update_raw_el(|raw_el| {
        let mut raw_el = raw_el;
        for slot in children {
            match slot {
                ChildSlot::Static(el) => {
                    raw_el = raw_el.child(el);
                }
                ChildSlot::Signal(sig) => {
                    raw_el = raw_el.child_signal(sig);
                }
            }
        }
        let raw_el = apply_raw_css(raw_el, style, program, instance, ctx.item_ctx(), false);
        apply_physical_css(raw_el, style, program, instance, ctx.item_ctx())
    })
    .into_raw_unchecked()
}

/// Build a Link element.
fn build_link(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    label: &IrExpr,
    url: &IrExpr,
    style: &IrExpr,
    _links: &[(String, EventId)],
    hovered_cell: Option<CellId>,
) -> RawElOrText {
    let label_text = resolve_static_text(program, label);
    let url_text = resolve_static_text(program, url);
    let mut lnk = Link::new().to(&url_text).new_tab(NewTab::new());
    lnk = apply_typed_styles(lnk, style, program, false);
    if let Some(cell) = hovered_cell {
        lnk = apply_hover(lnk, instance, None, cell);
    }
    lnk.label(zoon::Text::new(label_text))
        .update_raw_el(|raw_el| {
            let raw_el = apply_raw_css(raw_el, style, program, instance, None, false);
            apply_physical_css(raw_el, style, program, instance, None)
        })
        .into_raw_unchecked()
}

/// Build a Paragraph element.
fn build_paragraph(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    content: &IrExpr,
    style: &IrExpr,
    hovered_cell: Option<CellId>,
) -> RawElOrText {
    // Build children based on content type.
    let children: Vec<RawElOrText> = match content {
        IrExpr::TextConcat(segments) => {
            vec![build_text_from_segments(instance, segments)]
        }
        IrExpr::CellRead(cell) => {
            if let Some(node) = find_node_for_cell(program, *cell) {
                match node {
                    IrNode::Derived {
                        expr: IrExpr::ListConstruct(items),
                        ..
                    } => items
                        .iter()
                        .map(|item| match item {
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
                        })
                        .collect(),
                    _ => {
                        vec![build_cell_element(program, instance, *cell)]
                    }
                }
            } else {
                vec![build_reactive_text(instance, *cell)]
            }
        }
        IrExpr::ListConstruct(items) => items
            .iter()
            .map(|item| match item {
                IrExpr::CellRead(child_cell) => build_cell_element(program, instance, *child_cell),
                IrExpr::Constant(IrValue::Text(t)) => zoon::Text::new(t.clone()).unify(),
                _ => {
                    let t = eval_static_text(item);
                    zoon::Text::new(t).unify()
                }
            })
            .collect(),
        _ => {
            let text = eval_static_text(content);
            vec![zoon::Text::new(text).unify()]
        }
    };
    let mut para = Paragraph::new();
    para = apply_typed_styles(para, style, program, false);
    if let Some(cell) = hovered_cell {
        para = apply_hover(para, instance, None, cell);
    }
    para.contents(children)
        .update_raw_el(|raw_el| {
            let raw_el = apply_raw_css(raw_el, style, program, instance, None, false);
            apply_physical_css(raw_el, style, program, instance, None)
        })
        .into_raw_unchecked()
}

/// Build a Checkbox element using Zoon's typed Checkbox.
/// `.checked_signal()` drives visual state from the cell, `.on_change()` fires
/// the WASM event. Zoon's built-in click handler toggles internal state
/// momentarily, but the next signal update from the cell corrects it.
fn build_checkbox(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    checked: Option<&CellId>,
    style: &IrExpr,
    links: &[(String, EventId)],
    icon: Option<&CellId>,
    hovered_cell: Option<CellId>,
) -> RawElOrText {
    let click_event = links.iter().find(|(n, _)| n == "click").map(|(_, e)| *e);

    // Build icon element upfront so we don't need to capture &IrProgram in the closure.
    let icon_el: RawElOrText = if let Some(&icon_cell) = icon {
        let el = build_cell_element(program, instance, icon_cell);
        match el {
            RawElOrText::RawHtmlEl(el) => {
                RawElOrText::RawHtmlEl(el.style("pointer-events", "none"))
            }
            other => other,
        }
    } else {
        El::new().into_raw_unchecked()
    };

    // Drive checked state from cell signal.
    let checked_signal: LocalBoxSignal<'static, bool> = if let Some(&checked_cell) = checked {
        let store = instance.cell_store.clone();
        let cell_id = checked_cell.0;
        store
            .get_cell_signal(cell_id)
            .map(move |v| v != 0.0)
            .boxed_local()
    } else {
        always(false).boxed_local()
    };

    // on_change must NOT fire events: Zoon's on_change triggers for BOTH user clicks
    // AND programmatic checked_signal updates (they share internal Mutable<CheckState>).
    // This creates an infinite oscillation: fire_event → cell update → checked_signal →
    // on_change → fire_event. Instead, use a raw DOM click handler that only fires on
    // actual user interaction.
    let inst_change = instance.clone();
    let mut cb = Checkbox::new()
        .label_hidden("toggle")
        .checked_signal(checked_signal)
        .icon(move |_checked| icon_el)
        .on_change(move |_checked| {
            // No-op: event firing handled by raw click handler below.
        });

    cb = apply_typed_styles(cb, style, program, false);
    if let Some(cell) = hovered_cell {
        cb = apply_hover(cb, instance, None, cell);
    }
    cb.update_raw_el(|raw_el| {
        let raw_el = raw_el.event_handler(move |_: events::Click| {
            if let Some(event_id) = click_event {
                let _ = inst_change.fire_event(event_id.0);
            }
        });
        let raw_el = apply_raw_css(raw_el, style, program, instance, None, false);
        apply_physical_css(raw_el, style, program, instance, None)
    })
    .into_raw_unchecked()
}

/// Build a WHILE element — reactively switches child based on source cell value.
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
                matches!(
                    node,
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
        // WHILE/WHEN arms that produce ListConstruct (e.g., conditional stripe items).
        IrExpr::ListConstruct(_) => true,
        _ => false,
    }
}

/// Build an element from a WHEN/WHILE arm body expression.
/// Returns `None` for NoElement arms (element should be removed from DOM).
fn build_arm_body_element(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    ctx: &BuildContext,
    body: &IrExpr,
    source: CellId,
) -> Option<RawElOrText> {
    match body {
        IrExpr::CellRead(cell) => {
            if is_no_element(program, *cell) {
                None
            } else {
                Some(build_element(program, instance, ctx, *cell))
            }
        }
        IrExpr::Constant(IrValue::Tag(t)) if t == "NoElement" => None,
        IrExpr::ListConstruct(items) => {
            // Conditional arm produces a list of children (e.g., WHILE switching stripe items).
            // Wrap in a display:contents div so the parent stripe gets flat children.
            let children: Vec<RawElOrText> = items
                .iter()
                .filter_map(|item| match item {
                    IrExpr::CellRead(cell) => {
                        Some(build_element(program, instance, ctx, *cell))
                    }
                    IrExpr::Constant(IrValue::Tag(t)) if t == "NoElement" => None,
                    _ => None,
                })
                .collect();
            if children.is_empty() {
                None
            } else {
                Some(
                    RawHtmlEl::new("div")
                        .style("display", "contents")
                        .children(children)
                        .into_raw_unchecked(),
                )
            }
        }
        IrExpr::TextConcat(segments) => match ctx {
            BuildContext::Item(item_ctx) => {
                Some(build_item_text_from_segments(instance, item_ctx, segments))
            }
            BuildContext::Global => Some(build_text_from_segments(instance, segments)),
        },
        _ => {
            let text = eval_static_text(body);
            if text.is_empty() {
                Some(build_reactive_text_ctx(instance, ctx, source))
            } else {
                Some(zoon::Text::new(text).unify())
            }
        }
    }
}

/// Build a conditional element (WHEN/WHILE) for non-Stripe contexts.
/// Uses `El` with `child_signal` to reactively swap content.
fn build_conditional_element(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    ctx: &BuildContext,
    source: CellId,
    arms: &[(IrPattern, IrExpr)],
) -> RawElOrText {
    // Single arm optimization: build directly, no signal needed.
    if arms.len() == 1 {
        return build_arm_body_element(program, instance, ctx, &arms[0].1, source)
            .unwrap_or_else(|| El::new().unify());
    }
    El::new()
        .child_signal(build_conditional_signal(
            program, instance, ctx, source, arms,
        ))
        .unify()
}

/// Build a reactive signal for a WHEN/WHILE conditional.
/// Produces `Some(element)` for matched arms or `None` for NoElement/unmatched.
fn build_conditional_signal(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    ctx: &BuildContext,
    source: CellId,
    arms: &[(IrPattern, IrExpr)],
) -> Box<dyn Signal<Item = Option<RawElOrText>> + Unpin> {
    // Extract item context as owned data, then delegate to the inner function
    // which has no BuildContext<'a> parameter. This prevents the compiler from
    // thinking the returned Box<dyn Signal> captures BuildContext's lifetime.
    let item_ctx: Option<ItemContext> = match *ctx {
        BuildContext::Global => None,
        BuildContext::Item(ic) => Some(ic.clone()),
    };
    build_conditional_signal_impl(program, instance, item_ctx, source, arms)
}

fn build_conditional_signal_impl(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    item_ctx: Option<ItemContext>,
    source: CellId,
    arms: &[(IrPattern, IrExpr)],
) -> Box<dyn Signal<Item = Option<RawElOrText>> + Unpin> {
    let tag_table = &program.tag_table;
    let matchers: Vec<ArmMatcher> = arms
        .iter()
        .map(|(p, _)| pattern_to_matcher(p, tag_table))
        .collect();
    let arm_bodies: Vec<IrExpr> = arms.iter().map(|(_, body)| body.clone()).collect();
    let inst = instance.clone();
    let source_id = source.0;

    if let Some(item_ctx) = item_ctx {
        let is_item_source = item_ctx.is_template_cell(source);
        let store = instance.cell_store.clone();
        let item_cell_store = instance.item_cell_store.clone();

        let signal: Box<dyn Signal<Item = f64> + Unpin> = if is_item_source {
            let ics = instance.item_cell_store.clone().unwrap();
            Box::new(ics.get_signal(item_ctx.item_idx, source_id))
        } else {
            Box::new(instance.cell_store.get_cell_signal(source_id))
        };

        Box::new(signal.map(move |_val| {
            let (val, text) = if is_item_source {
                if let Some(ref ics) = item_cell_store {
                    (
                        ics.get_value(item_ctx.item_idx, source_id),
                        ics.get_text(item_ctx.item_idx, source_id),
                    )
                } else {
                    (
                        store.get_cell_value(source_id),
                        store.get_cell_text(source_id),
                    )
                }
            } else {
                (
                    store.get_cell_value(source_id),
                    store.get_cell_text(source_id),
                )
            };
            let matched = find_matching_arm_idx(&matchers, val, &text);
            matched.and_then(|idx| {
                let build_ctx = BuildContext::Item(&item_ctx);
                build_arm_body_element(
                    &inst.program,
                    &inst,
                    &build_ctx,
                    &arm_bodies[idx],
                    CellId(source_id),
                )
            })
        }))
    } else {
        let store = instance.cell_store.clone();
        Box::new(store.get_cell_signal(source_id).map(move |_val| {
            let val = store.get_cell_value(source_id);
            let text = store.get_cell_text(source_id);
            let matched = find_matching_arm_idx(&matchers, val, &text);
            matched.and_then(|idx| {
                build_arm_body_element(
                    &inst.program,
                    &inst,
                    &BuildContext::Global,
                    &arm_bodies[idx],
                    CellId(source_id),
                )
            })
        }))
    }
}

/// Build per-item conditional children with visibility toggling.
/// Elements are always in the DOM — only visibility changes. This prevents hover
/// oscillation caused by DOM structural changes (child_signal's removeChild/insertBefore).
/// Unlike the old opacity approach, `visibility: hidden` also disables pointer events,
/// so hidden elements don't interfere with hover detection on siblings.
fn build_item_visibility_children(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    ctx: &ItemContext,
    source: CellId,
    arms: &[(IrPattern, IrExpr)],
) -> Vec<RawElOrText> {
    let tag_table = &program.tag_table;
    let matchers: Vec<ArmMatcher> = arms
        .iter()
        .map(|(p, _)| pattern_to_matcher(p, tag_table))
        .collect();
    let source_id = source.0;
    let is_item_source = ctx.is_template_cell(source);
    let item_idx = ctx.item_idx;

    let mut result = Vec::new();
    for (arm_idx, (_, body)) in arms.iter().enumerate() {
        let el =
            match build_arm_body_element(program, instance, &BuildContext::Item(ctx), body, source)
            {
                Some(el) => el,
                None => continue,
            };

        // Apply visibility directly on the element (no wrapper div).
        let html_el = match el {
            RawElOrText::RawHtmlEl(html_el) => html_el,
            other => {
                result.push(other);
                continue;
            }
        };

        let matchers = matchers.clone();
        let store = instance.cell_store.clone();
        let ics = instance.item_cell_store.clone();

        let signal: Box<dyn Signal<Item = f64> + Unpin> = if is_item_source {
            Box::new(
                instance
                    .item_cell_store
                    .clone()
                    .unwrap()
                    .get_signal(item_idx, source_id),
            )
        } else {
            Box::new(instance.cell_store.get_cell_signal(source_id))
        };

        let visibility_signal = signal.map(move |_| {
            let (val, text) = if is_item_source {
                if let Some(ref ics) = ics {
                    (
                        ics.get_value(item_idx, source_id),
                        ics.get_text(item_idx, source_id),
                    )
                } else {
                    (
                        store.get_cell_value(source_id),
                        store.get_cell_text(source_id),
                    )
                }
            } else {
                (
                    store.get_cell_value(source_id),
                    store.get_cell_text(source_id),
                )
            };
            if find_matching_arm_idx(&matchers, val, &text) == Some(arm_idx) {
                "visible"
            } else {
                "hidden"
            }
        });

        result.push(
            html_el
                .style("visibility", "hidden")
                .style_signal("visibility", visibility_signal)
                .into_raw_unchecked(),
        );
    }
    result
}

/// Check if any arm in a conditional is NoElement.
fn has_no_element_arm(program: &IrProgram, arms: &[(IrPattern, IrExpr)]) -> bool {
    arms.iter().any(|(_, body)| match body {
        IrExpr::Constant(IrValue::Tag(t)) if t == "NoElement" => true,
        IrExpr::CellRead(cell) => is_no_element(program, *cell),
        _ => false,
    })
}

/// Pattern matcher for arm selection.
#[derive(Clone, Debug)]
enum ArmMatcher {
    Tag(f64),     // Match encoded tag value
    Number(f64),  // Match exact number
    Bool(f64),    // Match 0.0 or 1.0
    Text(String), // Match cell text value
    Wildcard,     // Match anything
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
    find_matching_arm_idx(
        &arms.iter().map(|(m, _)| m.clone()).collect::<Vec<_>>(),
        value,
        cell_text,
    )
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
    item_cell_id: u32,
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

/// Context for building elements — Global (top-level) or Item (per-list-item).
#[derive(Clone)]
enum BuildContext<'a> {
    Global,
    Item(&'a ItemContext),
}

impl<'a> BuildContext<'a> {
    fn item_ctx(&self) -> Option<&ItemContext> {
        match self {
            BuildContext::Item(ctx) => Some(ctx),
            _ => None,
        }
    }

    fn is_template_cell(&self, cell: CellId) -> bool {
        match self {
            BuildContext::Item(ctx) => ctx.is_template_cell(cell),
            _ => false,
        }
    }

    fn is_template_event(&self, event: EventId) -> bool {
        match self {
            BuildContext::Item(ctx) => ctx.is_template_event(event),
            _ => false,
        }
    }

    fn fire_event(&self, instance: &Rc<WasmInstance>, event: EventId) {
        match self {
            BuildContext::Item(ctx) if ctx.is_template_event(event) => {
                let _ = instance.call_on_item_event(ctx.item_idx, event.0);
            }
            _ => {
                let _ = instance.fire_event(event.0);
            }
        }
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
        IrNode::Derived {
            expr: IrExpr::CellRead(cell),
            ..
        } => Some(*cell),
        _ => None,
    };

    // Collect cross-scope event IDs (global events that trigger template-scoped nodes).
    // These need to be forwarded to all relevant items when the global event fires.
    let cross_scope_events =
        collect_cross_scope_events(program, template_cell_range, template_event_range);

    // For fanout, prefer the backing (unfiltered) source list. This keeps
    // cross-scope item updates (e.g. toggle-all) working even when the map's
    // visible source is filtered to zero items.
    let fanout_source = resolve_cross_scope_fanout_source(program, source);

    let inst = instance.clone();

    // Track which items have been initialized by their stable memory index.
    // On re-renders (e.g. filter changes), existing items keep their HOLD state;
    // only items not yet seen get init_item called.
    let initialized_indices: Rc<RefCell<HashSet<u32>>> = Rc::new(RefCell::new(HashSet::new()));

    // Set up cross-scope event forwarding: when a global event fires,
    // forward it to all backing-list items via on_item_event.
    if !cross_scope_events.is_empty() {
        let cross_events = cross_scope_events.clone();
        let inst_hook = inst.clone();
        let initialized_for_hook = initialized_indices.clone();
        let hook_map_cell = map_cell.0;
        inst.add_post_event_hook(Box::new(move |event_id| {
            if cross_events.contains(&event_id) {
                let list_id = inst_hook.cell_store.get_cell_value(fanout_source.0);
                let text_items = inst_hook.list_store.items_text(list_id);
                let f64_items = inst_hook.list_store.items(list_id);
                let item_count = if !text_items.is_empty() {
                    text_items.len()
                } else {
                    f64_items.len()
                };

                for pos in 0..item_count {
                    let item_idx = inst_hook.list_store.item_memory_index(list_id, pos) as u32;
                    if let Some(ref ics) = inst_hook.item_cell_store {
                        ics.ensure_item(item_idx);
                    }
                    let should_init = {
                        let initialized = initialized_for_hook.borrow();
                        !initialized.contains(&item_idx)
                    };
                    if should_init {
                        let _ = inst_hook.call_init_item(item_idx, hook_map_cell);
                        initialized_for_hook.borrow_mut().insert(item_idx);
                    }
                    // Use batch version — fire_event calls rerun_retain_filters once at end.
                    let _ = inst_hook.call_on_item_event_batch(item_idx, event_id);
                }
            }
        }));
    }

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
        let item_count = if !text_items.is_empty() {
            text_items.len()
        } else {
            f64_items.len()
        };
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

    // Build a map: field_name → HOLD init source cell for each field in item_cell.
    // This allows us to seed the correct text on the HOLD's source cell BEFORE
    // init_item runs, so that host_copy_text propagates the right text through
    // the entire template (including derived text interpolations like labels).
    let field_hold_sources: HashMap<String, CellId> = program
        .cell_field_cells
        .get(&item_cell)
        .map(|fields| {
            fields
                .iter()
                .filter_map(|(name, field_cell)| {
                    // Find the Hold node for this field cell.
                    program.nodes.iter().find_map(|node| {
                        if let IrNode::Hold { cell, init, .. } = node {
                            if *cell == *field_cell {
                                if let IrExpr::CellRead(src) = init {
                                    return Some((name.clone(), *src));
                                }
                            }
                        }
                        None
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    // Build a container that reactively re-renders children when the list changes.
    RawHtmlEl::new("div")
        .style("display", "contents")
        .child_signal(deduped_signal.map(move |_opt| {
            // Re-read list_id from source cell each time — the filter loop may
            // have replaced the list with a new filtered copy.
            let current_list_id = inst.cell_store.get_cell_value(source.0);
            let text_items = inst.list_store.items_text(current_list_id);
            let f64_items = inst.list_store.items(current_list_id);
            let item_count = if !text_items.is_empty() {
                text_items.len()
            } else {
                f64_items.len()
            };
            if item_count == 0 {
                // Only reset initialized indices when the backing list is truly
                // empty. A filtered view can be empty while backing items still
                // exist and must keep their per-item state.
                let backing_list_id = inst.cell_store.get_cell_value(fanout_source.0);
                let backing_text_items = inst.list_store.items_text(backing_list_id);
                let backing_f64_items = inst.list_store.items(backing_list_id);
                let backing_count = if !backing_text_items.is_empty() {
                    backing_text_items.len()
                } else {
                    backing_f64_items.len()
                };
                if backing_count == 0 {
                    initialized_indices.borrow_mut().clear();
                }
                return None;
            }

            let program = &inst.program;
            let has_pending_snapshot = inst.has_pending_snapshot();

            let children: Vec<RawElOrText> = (0..item_count)
                .map(|i| {
                    // Use item_memory_index to correctly map position → memory index.
                    // For index-based lists (after ListRetain/ListRemove), f64 values
                    // are original memory indices. For regular lists, use sequential index.
                    let item_idx = inst.list_store.item_memory_index(current_list_id, i) as u32;

                    if let Some(ref ics) = inst.item_cell_store {
                        ics.ensure_item(item_idx);
                    }

                    // Seed item text from ListStore BEFORE init_item so the
                    // WASM template code can read it via host_copy_text during
                    // initialization (e.g., copying item.title to display cells).
                    if let Some(ref ics) = inst.item_cell_store {
                        if !has_pending_snapshot || ics.get_text(item_idx, item_cell.0).is_empty() {
                            if i < text_items.len() {
                                ics.set_text(item_idx, item_cell.0, text_items[i].clone());
                            } else if i < f64_items.len() {
                                // Numeric list items: format value as text for label display.
                                ics.set_text(item_idx, item_cell.0, format_number(f64_items[i]));
                            }
                        }
                    }

                    // Seed HOLD source cells BEFORE init_item. For multi-field
                    // objects (e.g., new_person with name + surname), the template's
                    // HOLD inits copy text from param source cells via host_copy_text.
                    // The first param is bound to item_cell (already seeded above).
                    // Remaining params are bound to default (False) cells. We set
                    // the correct per-field texts on those source cells so init_item
                    // propagates them through the entire template, including derived
                    // text interpolations (e.g., label = TEXT { {surname}, {name} }).
                    if !initialized_indices.borrow().contains(&item_idx) {
                        if let Some(ref ics) = inst.item_cell_store {
                            let field_texts = inst.list_store.field_texts_for_mem_idx(item_idx);
                            if !field_texts.is_empty() {
                                for (name, source_cell) in &field_hold_sources {
                                    if let Some(text) = field_texts.get(name) {
                                        if !text.is_empty() {
                                            // Set on ICS — init_item's host_copy_text
                                            // reads template cells from ICS.
                                            ics.set_text(item_idx, source_cell.0, text.clone());
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Initialize per-item template cells for this ListMap.
                    // Only init NEW items — existing items keep their HOLD state
                    // (e.g. completed toggle) across re-renders.
                    // Runs AFTER text seeding so host_copy_text reads correct texts.
                    if !initialized_indices.borrow().contains(&item_idx) {
                        let _ = inst.call_init_item(item_idx, map_cell.0);
                        initialized_indices.borrow_mut().insert(item_idx);
                    }

                    // Build per-item element tree.
                    let ctx = ItemContext {
                        item_idx,
                        item_cell_id: item_cell.0,
                        template_cell_range,
                        template_event_range,
                    };

                    if let Some(root_cell) = template_root_cell {
                        build_element(program, &inst, &BuildContext::Item(&ctx), root_cell)
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
                })
                .collect();

            // Finalize snapshot restore: re-derive global values from
            // restored per-item WASM memory and re-apply global cells.
            inst.finalize_restore();

            // Enable persistence saving now that all items are initialized.
            // This prevents premature saves (e.g., from router events during startup)
            // before per-item state is ready.
            inst.enable_save();

            Some(
                RawHtmlEl::new("div")
                    .style("display", "contents")
                    .children(children)
                    .into_raw_unchecked(),
            )
        }))
        .into_raw_unchecked()
}

/// Resolve the list cell to use for cross-scope per-item event fanout.
///
/// For filtered views (`ListRetain -> ListMap`), use the retain's source so
/// hidden items still receive global events (e.g. toggle-all in TodoMVC).
/// We unwrap at most one retain layer plus pass-through wrappers.
fn resolve_cross_scope_fanout_source(program: &IrProgram, source: CellId) -> CellId {
    let mut current = source;
    let mut seen = HashSet::new();
    let mut unwrapped_retain = false;

    while seen.insert(current) {
        match find_node_for_cell(program, current) {
            Some(IrNode::Derived {
                expr: IrExpr::CellRead(next),
                ..
            }) => {
                current = *next;
            }
            Some(IrNode::PipeThrough { source: next, .. }) => {
                current = *next;
            }
            Some(IrNode::ListRetain { source: next, .. }) if !unwrapped_retain => {
                current = *next;
                unwrapped_retain = true;
            }
            _ => break,
        }
    }

    current
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
            IrNode::Hold {
                cell,
                trigger_bodies,
                ..
            } if cell.0 >= cell_start && cell.0 < cell_end => {
                trigger_bodies.iter().map(|(t, _)| t.0).collect()
            }
            IrNode::Then { cell, trigger, .. } if cell.0 >= cell_start && cell.0 < cell_end => {
                vec![trigger.0]
            }
            IrNode::Latest { target, arms } if target.0 >= cell_start && target.0 < cell_end => {
                arms.iter()
                    .filter_map(|arm| arm.trigger.map(|t| t.0))
                    .collect()
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
        let val = ics.get_value(item_idx, cell_id);
        if !text.is_empty() {
            text
        } else {
            format_number(val)
        }
    }))
    .unify()
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
    let seg_desc: Vec<ItemSegDesc> = segments
        .iter()
        .map(|seg| match seg {
            TextSegment::Literal(t) => ItemSegDesc::Lit(t.clone()),
            TextSegment::Expr(IrExpr::CellRead(cell)) => {
                if ctx.is_template_cell(*cell) {
                    ItemSegDesc::ItemCell(cell.0)
                } else {
                    ItemSegDesc::GlobalCell(cell.0)
                }
            }
            _ => ItemSegDesc::Lit(String::new()),
        })
        .collect();

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
            seg_desc
                .iter()
                .map(|s| match s {
                    ItemSegDesc::Lit(t) => t.clone(),
                    ItemSegDesc::ItemCell(id) => {
                        let text = ics.get_text(item_idx, *id);
                        if !text.is_empty() {
                            text
                        } else {
                            format_number(ics.get_value(item_idx, *id))
                        }
                    }
                    ItemSegDesc::GlobalCell(id) => format_cell_value(&store, *id),
                })
                .collect::<String>()
        }))
        .unify()
    } else {
        let signal = store.get_cell_signal(trigger_cell.0);
        zoon::Text::with_signal(signal.map(move |_| {
            seg_desc
                .iter()
                .map(|s| match s {
                    ItemSegDesc::Lit(t) => t.clone(),
                    ItemSegDesc::ItemCell(id) => {
                        let text = ics.get_text(item_idx, *id);
                        if !text.is_empty() {
                            text
                        } else {
                            format_number(ics.get_value(item_idx, *id))
                        }
                    }
                    ItemSegDesc::GlobalCell(id) => format_cell_value(&store, *id),
                })
                .collect::<String>()
        }))
        .unify()
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
    // Track which event names are handled directly by the element builder
    // so attach_item_events doesn't duplicate them.
    let mut handled_events: &[&str] = &[];

    // Typed element builders handle hover via .on_hovered_change() directly.
    // Only Checkbox (RawHtmlEl) still needs attach_item_events for hover.
    let mut remaining_hovered_cell = None;

    let raw: RawElOrText = match kind {
        ElementKind::Button { label, style } => build_button(
            program,
            instance,
            &BuildContext::Item(ctx),
            label,
            style,
            links,
            hovered_cell,
        ),
        ElementKind::Stripe {
            direction,
            items,
            gap,
            style,
            ..
        } => build_stripe(
            program,
            instance,
            &BuildContext::Item(ctx),
            direction,
            *items,
            gap,
            style,
            hovered_cell,
        ),
        ElementKind::TextInput {
            placeholder,
            style,
            focus,
            text_cell: reactive_text_cell,
        } => build_item_text_input(
            program,
            instance,
            ctx,
            placeholder.as_ref(),
            style,
            links,
            *focus,
            *reactive_text_cell,
            hovered_cell,
        ),
        ElementKind::Checkbox {
            checked,
            style,
            icon,
        } => {
            handled_events = &["click"];
            build_item_checkbox(
                program,
                instance,
                ctx,
                checked.as_ref(),
                style,
                links,
                icon.as_ref(),
                hovered_cell,
            )
        }
        ElementKind::Container { child, style } => build_container(
            program,
            instance,
            &BuildContext::Item(ctx),
            *child,
            style,
            hovered_cell,
        ),
        ElementKind::Label { label, style } => build_label(
            program,
            instance,
            &BuildContext::Item(ctx),
            label,
            style,
            links,
            hovered_cell,
        ),
        ElementKind::Stack { layers, style } => build_stack(
            program,
            instance,
            &BuildContext::Item(ctx),
            *layers,
            style,
            hovered_cell,
        ),
        ElementKind::Link { url, label, style } => {
            // Per-item Link reuses global builder; hover is deferred to attach_item_events.
            remaining_hovered_cell = hovered_cell;
            build_link(program, instance, label, url, style, links, None)
        }
        ElementKind::Paragraph { content, style } => {
            remaining_hovered_cell = hovered_cell;
            build_paragraph(program, instance, content, style, None)
        }
        ElementKind::Block { child, style } => build_block(
            program,
            instance,
            &BuildContext::Item(ctx),
            *child,
            style,
            hovered_cell,
        ),
        ElementKind::Text { label, style } => build_text_element(
            program,
            instance,
            &BuildContext::Item(ctx),
            label,
            style,
            hovered_cell,
        ),
        ElementKind::Slider {
            style,
            value_cell,
            min,
            max,
            step,
        } => build_slider(
            program,
            instance,
            style,
            links,
            *value_cell,
            *min,
            *max,
            *step,
            hovered_cell,
        ),
        ElementKind::Select {
            style,
            options,
            selected,
        } => build_select(
            program,
            instance,
            style,
            links,
            options,
            selected.as_ref(),
            hovered_cell,
        ),
        ElementKind::Svg { style, children } => build_svg(
            program,
            instance,
            &BuildContext::Item(ctx),
            style,
            *children,
            links,
            hovered_cell,
        ),
        ElementKind::SvgCircle { cx, cy, r, style } => build_svg_circle(
            program,
            instance,
            &BuildContext::Item(ctx),
            cx,
            cy,
            r,
            style,
        ),
    };
    // Attach remaining event handlers (blur, focus, click, double_click, and hover for Checkbox/Link/Paragraph).
    if handled_events.is_empty() {
        attach_item_events(raw, instance, ctx, links, remaining_hovered_cell)
    } else {
        let filtered: Vec<_> = links
            .iter()
            .filter(|(name, _)| !handled_events.contains(&name.as_str()))
            .cloned()
            .collect();
        attach_item_events(raw, instance, ctx, &filtered, remaining_hovered_cell)
    }
}

/// Attach event handlers to a per-item element.
/// Template-scoped events route through on_item_event; global events through on_event.
/// Events are attached directly to the element (no wrapper div) to avoid
/// Chrome's buggy mouse events on `display: contents` elements.
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
    let double_click_event = links
        .iter()
        .find(|(n, _)| n == "double_click")
        .map(|(_, e)| *e);

    if !has_hover
        && blur_event.is_none()
        && focus_event.is_none()
        && click_event.is_none()
        && double_click_event.is_none()
    {
        return el;
    }

    // Extract the HTML element for direct event attachment (text nodes can't have events).
    let mut html_el = match el {
        RawElOrText::RawHtmlEl(html_el) => html_el,
        other => return other,
    };

    // Hover: use per-item cell if in template range.
    if let Some(cell) = hovered_cell {
        if ctx.is_template_cell(cell) {
            let ics = instance.item_cell_store.clone();
            let item_idx = ctx.item_idx;
            html_el = html_el.event_handler(move |_: events::MouseEnter| {
                if let Some(ref ics) = ics {
                    ics.set_cell(item_idx, cell.0, 1.0);
                }
            });
            let ics = instance.item_cell_store.clone();
            html_el = html_el.event_handler(move |_: events::MouseLeave| {
                if let Some(ref ics) = ics {
                    ics.set_cell(item_idx, cell.0, 0.0);
                }
            });
        } else {
            let inst = instance.clone();
            let cell_id = cell.0;
            html_el = html_el.event_handler(move |_: events::MouseEnter| {
                inst.set_cell_value(cell_id, 1.0);
            });
            let inst = instance.clone();
            let cell_id = cell.0;
            html_el = html_el.event_handler(move |_: events::MouseLeave| {
                inst.set_cell_value(cell_id, 0.0);
            });
        }
    }

    // Event helpers: route to on_item_event or on_event based on scope.
    let item_idx = ctx.item_idx;

    if let Some(event_id) = blur_event {
        let inst = instance.clone();
        let is_template = ctx.is_template_event(event_id);
        html_el = html_el.event_handler(move |_: events::Blur| {
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
        html_el = html_el.event_handler(move |_: events::Focus| {
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
        html_el = html_el.event_handler(move |_: events::Click| {
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
        html_el = html_el.event_handler(move |_: events::DoubleClick| {
            if is_template {
                let _ = inst.call_on_item_event(item_idx, event_id.0);
            } else {
                let _ = inst.fire_event(event_id.0);
            }
        });
    }

    html_el.into_raw_unchecked()
}

/// Apply hover handling on a typed element via `.on_hovered_change()`.
/// Routes to ItemCellStore for template cells, WasmInstance for global cells.
fn apply_hover<T: MouseEventAware>(
    el: T,
    instance: &Rc<WasmInstance>,
    item_ctx: Option<&ItemContext>,
    cell: CellId,
) -> T {
    match item_ctx {
        Some(ctx) if ctx.is_template_cell(cell) => {
            let ics = instance.item_cell_store.clone();
            let item_idx = ctx.item_idx;
            el.on_hovered_change(move |hovered| {
                if let Some(ref ics) = ics {
                    ics.set_cell(item_idx, cell.0, if hovered { 1.0 } else { 0.0 });
                }
            })
        }
        _ => {
            let inst = instance.clone();
            el.on_hovered_change(move |hovered| {
                inst.set_cell_value(cell.0, if hovered { 1.0 } else { 0.0 });
            })
        }
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
    hovered_cell: Option<CellId>,
) -> RawElOrText {
    let key_down_event = links
        .iter()
        .find(|(name, _)| name == "key_down")
        .map(|(_, eid)| *eid);
    let change_event = links
        .iter()
        .find(|(name, _)| name == "change")
        .map(|(_, eid)| *eid);
    let tmpl_range = Some(ctx.template_cell_range);
    let key_data_cell =
        find_data_cell_for_event_in_range(program, links, "key_down", "key", tmpl_range);
    let change_text_cell =
        find_data_cell_for_event_in_range(program, links, "change", "text", tmpl_range);
    let text_cell = find_text_property_cell_in_range(program, links, tmpl_range);

    let item_idx = ctx.item_idx;
    let ics_clone = instance.item_cell_store.clone();

    // Set initial input value from the reactive text cell (e.g., LATEST target)
    // or the .text property cell. For edit inputs, this pre-fills the input
    // with the current title.
    let initial_text_for_insert = {
        let cells_to_try: Vec<u32> = reactive_text_cell
            .iter()
            .map(|c| c.0)
            .chain(text_cell.iter().copied())
            .collect();
        let mut found_text: Option<String> = None;
        for cell_id in cells_to_try {
            let in_template_range =
                cell_id >= ctx.template_cell_range.0 && cell_id < ctx.template_cell_range.1;

            let mut candidates = Vec::with_capacity(2);
            if in_template_range {
                if let Some(ref ics) = instance.item_cell_store {
                    candidates.push(ics.get_text(item_idx, cell_id));
                }
                candidates.push(instance.cell_store.get_cell_text(cell_id));
            } else {
                candidates.push(instance.cell_store.get_cell_text(cell_id));
                if let Some(ref ics) = instance.item_cell_store {
                    candidates.push(ics.get_text(item_idx, cell_id));
                }
            }

            if let Some(text) = candidates.into_iter().find(|t| !t.is_empty()) {
                found_text = Some(text);
                break;
            }
        }

        if found_text.is_none() {
            if let Some(ref ics) = instance.item_cell_store {
                let item_text = ics.get_text(item_idx, ctx.item_cell_id);
                if !item_text.is_empty() {
                    found_text = Some(item_text);
                }
            }
        }

        found_text
    };
    let inherit_text_color = !style_defines_font_color(style, program);

    let mut ti = TextInput::new();
    ti = apply_typed_styles(ti, style, program, false);
    if let Some(cell) = hovered_cell {
        ti = apply_hover(ti, instance, Some(ctx), cell);
    }
    // Apply placeholder via Zoon's typed Placeholder API.
    let ph_el = build_placeholder(placeholder, program);
    ti.placeholder(ph_el)
        .update_raw_el(|raw_el| {
            let mut raw_el = raw_el
                .style("box-sizing", "border-box")
                .style("outline", "none");

            if inherit_text_color {
                raw_el = raw_el.style("color", "inherit");
            }

            raw_el = apply_raw_css(raw_el, style, program, instance, Some(ctx), false);
            raw_el = apply_physical_css(raw_el, style, program, instance, Some(ctx));

            // Set initial value and/or focus via after_insert.
            if focus || initial_text_for_insert.is_some() {
                if focus {
                    raw_el = raw_el.attr("autofocus", "");
                }
                raw_el = raw_el.after_insert(move |el| {
                    if let Some(ref text) = initial_text_for_insert {
                        el.set_value(text);
                    }
                    if focus {
                        let _ = el.focus();
                    }
                });
            }

            // Set up keydown event listener.
            let raw_el = if let Some(event_id) = key_down_event {
                let inst = instance.clone();
                let is_template = ctx.is_template_event(event_id);
                let change_event_for_key = change_event;
                let change_is_template = change_event
                    .map(|eid| ctx.is_template_event(eid))
                    .unwrap_or(false);
                let ics = ics_clone.clone();
                let template_cell_range = ctx.template_cell_range;
                raw_el.event_handler(move |event: events::KeyDown| {
                    let key = event.key();
                    let tag_value = match key.as_str() {
                        "Enter" => inst.program_tag_index("Enter"),
                        "Escape" => inst.program_tag_index("Escape"),
                        _ => 0.0,
                    };
                    if let Some(target) = event.target() {
                        if let Ok(input) = target.dyn_into::<web_sys::HtmlInputElement>() {
                            let input_text = input.value();
                            for cell_id_opt in [change_text_cell, text_cell] {
                                if let Some(cell_id) = cell_id_opt {
                                    if cell_id >= template_cell_range.0
                                        && cell_id < template_cell_range.1
                                    {
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
                    // For edit-save flows, commit the latest text through the
                    // `change` pipeline before handling Enter key_down. Some
                    // browsers don't dispatch an input/change event for Enter.
                    if key == "Enter" {
                        if let Some(change_eid) = change_event_for_key {
                            if change_is_template {
                                let _ = inst.call_on_item_event(item_idx, change_eid.0);
                            } else {
                                let _ = inst.fire_event(change_eid.0);
                            }
                        }
                    }
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
            } else {
                raw_el
            };

            // Set up input change event listener.
            if let Some(event_id) = change_event {
                let inst = instance.clone();
                let is_template = ctx.is_template_event(event_id);
                let ics = ics_clone.clone();
                let template_cell_range = ctx.template_cell_range;
                raw_el.event_handler(move |event: events::Input| {
                    if let Some(target) = event.target() {
                        if let Ok(input) = target.dyn_into::<web_sys::HtmlInputElement>() {
                            let input_text = input.value();
                            for cell_id_opt in [change_text_cell, text_cell] {
                                if let Some(cell_id) = cell_id_opt {
                                    if cell_id >= template_cell_range.0
                                        && cell_id < template_cell_range.1
                                    {
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
            } else {
                raw_el
            }
        })
        .into_raw_unchecked()
}

/// Build a per-item Checkbox element using Zoon's typed Checkbox.
fn build_item_checkbox(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    ctx: &ItemContext,
    checked: Option<&CellId>,
    style: &IrExpr,
    links: &[(String, EventId)],
    icon: Option<&CellId>,
    hovered_cell: Option<CellId>,
) -> RawElOrText {
    let click_event = links.iter().find(|(n, _)| n == "click").map(|(_, e)| *e);
    let item_idx = ctx.item_idx;

    // Build icon element upfront so we don't need to capture &IrProgram in the closure.
    let icon_el: RawElOrText = if let Some(&icon_cell) = icon {
        let el = build_element(program, instance, &BuildContext::Item(ctx), icon_cell);
        match el {
            RawElOrText::RawHtmlEl(el) => {
                RawElOrText::RawHtmlEl(el.style("pointer-events", "none"))
            }
            other => other,
        }
    } else {
        El::new().into_raw_unchecked()
    };

    // Drive checked state from per-item cell signal.
    let checked_signal: LocalBoxSignal<'static, bool> = if let Some(&checked_cell) = checked {
        if let Some(ref ics) = instance.item_cell_store {
            let ics = ics.clone();
            let cell_id = checked_cell.0;
            ics.get_signal(item_idx, cell_id)
                .map(move |v| v != 0.0)
                .boxed_local()
        } else {
            always(false).boxed_local()
        }
    } else {
        always(false).boxed_local()
    };

    // on_change must NOT fire events — see build_checkbox comment for rationale.
    // Use raw DOM click handler to avoid checked_signal → on_change feedback loop.
    let inst_change = instance.clone();
    let is_template = click_event
        .map(|e| ctx.is_template_event(e))
        .unwrap_or(false);
    let mut cb = Checkbox::new()
        .label_hidden("toggle")
        .checked_signal(checked_signal)
        .icon(move |_checked| icon_el)
        .on_change(move |_checked| {
            // No-op: event firing handled by raw click handler below.
        });

    cb = apply_typed_styles(cb, style, program, false);
    if let Some(cell) = hovered_cell {
        cb = apply_hover(cb, instance, Some(ctx), cell);
    }
    cb.update_raw_el(|raw_el| {
        let raw_el = raw_el.event_handler(move |_: events::Click| {
            if let Some(event_id) = click_event {
                if is_template {
                    let _ = inst_change.call_on_item_event(item_idx, event_id.0);
                } else {
                    let _ = inst_change.fire_event(event_id.0);
                }
            }
        });
        let raw_el = apply_raw_css(raw_el, style, program, instance, Some(ctx), false);
        apply_physical_css(raw_el, style, program, instance, Some(ctx))
    })
    .into_raw_unchecked()
}

/// Build a per-item conditional element (WHEN/WHILE) using `child_signal`.
/// Elements are reactively inserted/removed from the DOM based on the source cell value.
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
            | IrNode::ListEvery { cell: c, .. }
            | IrNode::ListAny { cell: c, .. }
            | IrNode::ListIsEmpty { cell: c, .. }
            | IrNode::RouterGoTo { cell: c, .. }
            | IrNode::TextTrim { cell: c, .. }
            | IrNode::TextIsNotEmpty { cell: c, .. }
            | IrNode::TextToNumber { cell: c, .. }
            | IrNode::MathRound { cell: c, .. }
            | IrNode::TextStartsWith { cell: c, .. }
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

fn ir_node_type_name(node: &IrNode) -> String {
    match node {
        IrNode::Derived { expr, .. } => format!("Derived({:?})", std::mem::discriminant(expr)),
        IrNode::Hold { .. } => "Hold".into(),
        IrNode::Latest { .. } => "Latest".into(),
        IrNode::Then { .. } => "Then".into(),
        IrNode::When { source, .. } => format!("When(src={})", source.0),
        IrNode::While { source, .. } => format!("While(src={})", source.0),
        IrNode::Element { .. } => "Element".into(),
        IrNode::ListMap { .. } => "ListMap".into(),
        IrNode::PipeThrough { source, .. } => format!("PipeThrough(src={})", source.0),
        IrNode::HoldLoop { .. } => "HoldLoop".into(),
        IrNode::TextInterpolation { .. } => "TextInterpolation".into(),
        IrNode::TextTrim { .. } => "TextTrim".into(),
        _ => format!("{:?}", std::mem::discriminant(node)),
    }
}

/// Check if a cell resolves to the NoElement tag.
fn is_no_element(program: &IrProgram, cell: CellId) -> bool {
    if let Some(node) = find_node_for_cell(program, cell) {
        match node {
            IrNode::Derived {
                expr: IrExpr::Constant(IrValue::Tag(t)),
                ..
            } => t == "NoElement",
            IrNode::Derived {
                expr: IrExpr::CellRead(inner),
                ..
            } => is_no_element(program, *inner),
            _ => false,
        }
    } else {
        false
    }
}

/// Extract placeholder text from a placeholder expression.
/// Handles both simple text and complex objects like `[style: ..., text: TEXT { ... }]`.
fn extract_placeholder_text(expr: &IrExpr, program: &IrProgram) -> String {
    match expr {
        IrExpr::Constant(IrValue::Text(t)) => t.clone(),
        IrExpr::TextConcat(segs) => segs
            .iter()
            .map(|s| match s {
                TextSegment::Literal(t) => t.clone(),
                _ => String::new(),
            })
            .collect(),
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

/// Build a Placeholder for a TextInput. Always returns a Placeholder (empty if no text).
fn build_placeholder<'a>(placeholder: Option<&IrExpr>, program: &IrProgram) -> Placeholder<'a> {
    let Some(ph) = placeholder else {
        return Placeholder::new("");
    };
    let text = extract_placeholder_text(ph, program);
    let mut ph_el = Placeholder::new(text);
    if let Some((is_italic, color_css)) = extract_placeholder_font(ph, program) {
        let mut font = Font::new();
        if is_italic {
            font = font.italic();
        }
        if let Some(css) = color_css {
            font = font.color(css);
        }
        ph_el = ph_el.s(font);
    }
    ph_el
}

/// Extract placeholder font properties: (is_italic, optional_color_css).
/// Navigates through the placeholder object (possibly nested under "style")
/// to find font style and color.
fn extract_placeholder_font(expr: &IrExpr, program: &IrProgram) -> Option<(bool, Option<String>)> {
    let fields: Vec<(String, IrExpr)> = match expr {
        IrExpr::ObjectConstruct(fields) => {
            fields.iter().map(|(n, v)| (n.clone(), v.clone())).collect()
        }
        IrExpr::CellRead(cell) => reconstruct_object_fields(program, *cell),
        _ => return None,
    };
    extract_font_from_fields(&fields, program)
}

fn extract_font_from_fields(
    fields: &[(String, IrExpr)],
    program: &IrProgram,
) -> Option<(bool, Option<String>)> {
    for (name, val) in fields {
        if name == "style" {
            // Unwrap "style" wrapper and recurse.
            let inner = match val {
                IrExpr::ObjectConstruct(f) => {
                    f.iter().map(|(n, v)| (n.clone(), v.clone())).collect()
                }
                IrExpr::CellRead(cell) => reconstruct_object_fields(program, *cell),
                _ => continue,
            };
            return extract_font_from_fields(&inner, program);
        } else if name == "font" {
            let font_fields: Vec<(String, IrExpr)> = match val {
                IrExpr::ObjectConstruct(f) => {
                    f.iter().map(|(n, v)| (n.clone(), v.clone())).collect()
                }
                IrExpr::CellRead(cell) => reconstruct_object_fields(program, *cell),
                _ => continue,
            };
            let mut is_italic = false;
            let mut color_css = None;
            for (fname, fval) in &font_fields {
                match fname.as_str() {
                    "style" => {
                        if let IrExpr::Constant(IrValue::Tag(t)) = fval {
                            if t == "Italic" {
                                is_italic = true;
                            }
                        }
                    }
                    "color" => {
                        color_css = resolve_color(fval);
                    }
                    _ => {}
                }
            }
            return Some((is_italic, color_css));
        }
    }
    None
}

/// Evaluate a static text expression to a String.
fn eval_static_text(expr: &IrExpr) -> String {
    match expr {
        IrExpr::Constant(IrValue::Text(t)) => t.clone(),
        IrExpr::Constant(IrValue::Number(n)) => format_number(*n),
        IrExpr::Constant(IrValue::Tag(t)) => t.clone(),
        IrExpr::TextConcat(segs) => segs
            .iter()
            .map(|s| match s {
                TextSegment::Literal(t) => t.clone(),
                _ => String::new(),
            })
            .collect(),
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
    if depth > 20 {
        return String::new();
    }
    match expr {
        IrExpr::CellRead(cell) => {
            // Find the node for this cell and extract its expression.
            if let Some(node) = find_node_for_cell(program, *cell) {
                match node {
                    IrNode::Derived { expr: inner, .. } => {
                        resolve_static_text_depth(program, inner, depth + 1)
                    }
                    IrNode::TextInterpolation { parts, .. } => parts
                        .iter()
                        .map(|seg| match seg {
                            TextSegment::Literal(t) => t.clone(),
                            _ => String::new(),
                        })
                        .collect(),
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
    if depth > 20 {
        return String::new();
    }
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
    if depth > 20 {
        return None;
    }
    let node = find_node_for_cell(program, cell)?;
    match node {
        IrNode::Derived { expr, .. } => resolve_expr_constant(program, expr, depth + 1),
        _ => None,
    }
}

fn resolve_expr_constant(program: &IrProgram, expr: &IrExpr, depth: u32) -> Option<ConstValue> {
    if depth > 20 {
        return None;
    }
    match expr {
        IrExpr::Constant(IrValue::Tag(t)) => Some(ConstValue::Tag(t.clone())),
        IrExpr::Constant(IrValue::Number(n)) => Some(ConstValue::Number(*n)),
        IrExpr::Constant(IrValue::Text(t)) => Some(ConstValue::Text(t.clone())),
        IrExpr::CellRead(cell) => resolve_cell_constant(program, *cell, depth + 1),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
// Typed Zoon style API
// ---------------------------------------------------------------------------

/// Extract style fields from an IrExpr, handling both ObjectConstruct and
/// CellRead (object-store pattern).
fn extract_style_fields_vec<'a>(
    style: &'a IrExpr,
    program: &IrProgram,
    reconstructed_buf: &'a mut Vec<(String, IrExpr)>,
) -> &'a [(String, IrExpr)] {
    match style {
        IrExpr::ObjectConstruct(fields) => fields,
        IrExpr::CellRead(cell) => {
            *reconstructed_buf = reconstruct_object_fields(program, *cell);
            reconstructed_buf
        }
        IrExpr::Constant(IrValue::Void) => &[],
        _ => &[],
    }
}

/// Build a typed Width style from an IR dimension expression.
fn build_width(value: &IrExpr) -> Option<Width<'static>> {
    match value {
        IrExpr::Constant(IrValue::Number(n)) => Some(Width::exact(*n as u32)),
        IrExpr::Constant(IrValue::Tag(t)) if t == "Fill" => Some(Width::fill()),
        // width: [sizing: Fill/Number, minimum: Number, maximum: Number]
        IrExpr::ObjectConstruct(fields) => {
            let sizing = fields.iter().find(|(n, _)| n == "sizing");
            let minimum = fields.iter().find(|(n, _)| n == "minimum");
            let maximum = fields.iter().find(|(n, _)| n == "maximum");
            let mut w = match sizing.map(|(_, v)| v) {
                Some(IrExpr::Constant(IrValue::Tag(t))) if t == "Fill" => Width::fill(),
                Some(IrExpr::Constant(IrValue::Number(n))) => Width::exact(*n as u32),
                _ => return None,
            };
            if let Some((_, IrExpr::Constant(IrValue::Number(n)))) = minimum {
                w = w.min(*n as u32);
            }
            if let Some((_, IrExpr::Constant(IrValue::Number(n)))) = maximum {
                w = w.max(*n as u32);
            }
            Some(w)
        }
        _ => None,
    }
}

/// Build a typed Height style from an IR dimension expression.
fn build_height(value: &IrExpr) -> Option<Height<'static>> {
    match value {
        IrExpr::Constant(IrValue::Number(n)) => Some(Height::exact(*n as u32)),
        IrExpr::Constant(IrValue::Tag(t)) if t == "Fill" => Some(Height::fill()),
        IrExpr::Constant(IrValue::Tag(t)) if t == "Screen" => Some(Height::screen()),
        // height: [sizing: Fill/Number, minimum: Screen/Number, maximum: Number]
        IrExpr::ObjectConstruct(fields) => {
            let sizing = fields.iter().find(|(n, _)| n == "sizing");
            let minimum = fields.iter().find(|(n, _)| n == "minimum");
            let maximum = fields.iter().find(|(n, _)| n == "maximum");
            let mut h = match sizing.map(|(_, v)| v) {
                Some(IrExpr::Constant(IrValue::Tag(t))) if t == "Fill" => Height::fill(),
                Some(IrExpr::Constant(IrValue::Number(n))) => Height::exact(*n as u32),
                _ => return None,
            };
            match minimum.map(|(_, v)| v) {
                Some(IrExpr::Constant(IrValue::Tag(t))) if t == "Screen" => {
                    h = h.min_screen();
                }
                Some(IrExpr::Constant(IrValue::Number(n))) => {
                    h = h.min(*n as u32);
                }
                _ => {}
            }
            if let Some((_, IrExpr::Constant(IrValue::Number(n)))) = maximum {
                h = h.max(*n as u32);
            }
            Some(h)
        }
        _ => None,
    }
}

/// Build a typed Padding style from an IR padding object.
fn build_padding(value: &IrExpr) -> Option<Padding<'static>> {
    // Handle uniform padding: `padding: 12`
    if let IrExpr::Constant(IrValue::Number(n)) = value {
        let all = *n as u32;
        return Some(Padding::new().top(all).right(all).bottom(all).left(all));
    }
    // Handle directional padding: `padding: [row: X, column: Y]`
    if let IrExpr::ObjectConstruct(fields) = value {
        let mut top: Option<u32> = None;
        let mut bottom: Option<u32> = None;
        let mut left: Option<u32> = None;
        let mut right: Option<u32> = None;

        for (name, val) in fields {
            if let IrExpr::Constant(IrValue::Number(n)) = val {
                let px = *n as u32;
                match name.as_str() {
                    "top" => top = Some(px),
                    "bottom" => bottom = Some(px),
                    "left" => left = Some(px),
                    "right" => right = Some(px),
                    "row" => {
                        left = left.or(Some(px));
                        right = right.or(Some(px));
                    }
                    "column" => {
                        top = top.or(Some(px));
                        bottom = bottom.or(Some(px));
                    }
                    _ => {}
                }
            }
        }

        let t = top.unwrap_or(0);
        let r = right.unwrap_or(0);
        let b = bottom.unwrap_or(0);
        let l = left.unwrap_or(0);
        if t != 0 || r != 0 || b != 0 || l != 0 {
            return Some(Padding::new().top(t).right(r).bottom(b).left(l));
        }
    }
    None
}

/// Build a typed Gap style from an IR gap expression.
fn build_gap(value: &IrExpr) -> Option<Gap<'static>> {
    if let IrExpr::Constant(IrValue::Number(n)) = value {
        if *n > 0.0 {
            return Some(Gap::both(*n as u32));
        }
    }
    None
}

/// Build a typed RoundedCorners style.
fn build_rounded_corners(value: &IrExpr) -> Option<RoundedCorners> {
    if let IrExpr::Constant(IrValue::Number(n)) = value {
        return Some(RoundedCorners::all(*n as u32));
    }
    None
}

/// Build a typed Font style (static parts only — size, weight, family, align, italic).
/// Returns the Font and a flag indicating whether reactive styles (color, strikethrough)
/// must be applied via raw CSS because they need signals.
fn build_font_static(value: &IrExpr, program: &IrProgram) -> Option<Font<'static>> {
    let reconstructed;
    let fields: &[(String, IrExpr)] = match value {
        IrExpr::ObjectConstruct(fields) => fields,
        IrExpr::CellRead(cell) => {
            reconstructed = reconstruct_object_fields(program, *cell);
            &reconstructed
        }
        _ => return None,
    };

    let mut font = Font::new();
    let mut has_any = false;

    for (name, val) in fields {
        match name.as_str() {
            "size" => {
                if let IrExpr::Constant(IrValue::Number(n)) = val {
                    font = font.size(*n as u32);
                    has_any = true;
                } else if let Some(ConstValue::Number(n)) = resolve_expr_constant(program, val, 0) {
                    font = font.size(n as u32);
                    has_any = true;
                }
            }
            "color" => {
                // Only apply static color here — reactive colors handled separately.
                if let Some(css) = resolve_color(val) {
                    font = font.color(css);
                    has_any = true;
                }
                // else: reactive color, handled by apply_font_reactive
            }
            "weight" => {
                let tag = match val {
                    IrExpr::Constant(IrValue::Tag(t)) => Some(t.clone()),
                    _ => {
                        if let Some(ConstValue::Tag(t)) = resolve_expr_constant(program, val, 0) {
                            Some(t)
                        } else {
                            None
                        }
                    }
                };
                if let Some(t) = tag {
                    let w = match t.as_str() {
                        "ExtraLight" => FontWeight::ExtraLight,
                        "Light" => FontWeight::Light,
                        "Regular" | "Normal" => FontWeight::Regular,
                        "Medium" => FontWeight::Medium,
                        "SemiBold" => FontWeight::SemiBold,
                        "Bold" => FontWeight::Bold,
                        "ExtraBold" => FontWeight::ExtraBold,
                        _ => FontWeight::Regular,
                    };
                    font = font.weight(w);
                    has_any = true;
                }
            }
            "family" => {
                if let Some(family_css) = resolve_font_family(val, program) {
                    // Font::family takes FontFamily items. We need to pass raw CSS.
                    // Use FontFamily::new() with the full pre-formatted string.
                    // Actually, Zoon's FontFamily can be constructed with a string.
                    // But family() expects an IntoIterator<Item=FontFamily>.
                    // We can pass a single FontFamily::new(full_css) but that wraps in quotes.
                    // Instead, use raw CSS for font-family to avoid double-quoting.
                    // This is handled in apply_raw_font.
                }
            }
            "align" => {
                let tag = match val {
                    IrExpr::Constant(IrValue::Tag(t)) => Some(t.clone()),
                    _ => {
                        if let Some(ConstValue::Tag(t)) = resolve_expr_constant(program, val, 0) {
                            Some(t)
                        } else {
                            None
                        }
                    }
                };
                if let Some(t) = tag {
                    font = match t.as_str() {
                        "Center" => font.center(),
                        "Left" | "Start" => font.left(),
                        "Right" | "End" => font.right(),
                        _ => font.left(),
                    };
                    has_any = true;
                }
            }
            "style" => {
                if let IrExpr::Constant(IrValue::Tag(t)) = val {
                    if t == "Italic" {
                        font = font.italic();
                        has_any = true;
                    }
                }
            }
            "line" => {
                // Static strikethrough only — reactive handled in apply_font_reactive.
                let reconstructed_line;
                let line_fields: &[(String, IrExpr)] = match val {
                    IrExpr::ObjectConstruct(f) => f,
                    IrExpr::CellRead(cell) => {
                        reconstructed_line = reconstruct_object_fields(program, *cell);
                        &reconstructed_line
                    }
                    _ => continue,
                };
                for (lname, lval) in line_fields {
                    if lname == "strikethrough" {
                        if let IrExpr::Constant(IrValue::Bool(true)) = lval {
                            font = font.line(FontLine::new().strike());
                            has_any = true;
                        }
                        // CellRead → reactive, handled in apply_font_reactive
                    }
                }
            }
            _ => {}
        }
    }
    if has_any { Some(font) } else { None }
}

/// Build a typed Background style (static color + static URL only).
fn build_background_static(value: &IrExpr, program: &IrProgram) -> Option<Background<'static>> {
    let reconstructed;
    let fields: &[(String, IrExpr)] = match value {
        IrExpr::ObjectConstruct(fields) => fields,
        IrExpr::CellRead(cell) => {
            reconstructed = reconstruct_object_fields(program, *cell);
            &reconstructed
        }
        _ => return None,
    };

    let mut bg = Background::new();
    let mut has_any = false;

    for (name, val) in fields {
        match name.as_str() {
            "color" => {
                if let Some(css) = resolve_color_full(val) {
                    bg = bg.color(css);
                    has_any = true;
                }
            }
            "url" => {
                let url = eval_static_text(val);
                let url = if url.is_empty() {
                    resolve_static_text(program, val)
                } else {
                    url
                };
                if !url.is_empty() {
                    bg = bg.url(url).size(BackgroundSize::Contain);
                    has_any = true;
                }
                // Reactive URL handled in apply_background_reactive
            }
            _ => {}
        }
    }
    if has_any { Some(bg) } else { None }
}

/// Build a typed Borders style.
fn build_borders(value: &IrExpr) -> Option<Borders<'static>> {
    if let IrExpr::ObjectConstruct(fields) = value {
        let mut borders = Borders::new();
        let mut has_any = false;

        for (name, val) in fields {
            if let IrExpr::ObjectConstruct(border_fields) = val {
                let color_css = border_fields
                    .iter()
                    .find(|(n, _)| n == "color")
                    .and_then(|(_, v)| resolve_color_full(v))
                    .unwrap_or_else(|| "currentColor".to_string());
                let width = border_fields
                    .iter()
                    .find(|(n, _)| n == "width")
                    .and_then(|(_, v)| {
                        if let IrExpr::Constant(IrValue::Number(n)) = v {
                            Some(*n)
                        } else {
                            None
                        }
                    })
                    .unwrap_or(1.0);
                let border = Border::new().width(width as u32).color(color_css);
                match name.as_str() {
                    "top" => {
                        borders = borders.top(border);
                        has_any = true;
                    }
                    "bottom" => {
                        borders = borders.bottom(border);
                        has_any = true;
                    }
                    "left" => {
                        borders = borders.left(border);
                        has_any = true;
                    }
                    "right" => {
                        borders = borders.right(border);
                        has_any = true;
                    }
                    _ => {}
                }
            }
        }
        if has_any {
            return Some(borders);
        }
    }
    None
}

/// Build a typed Shadows style.
fn build_shadows(value: &IrExpr, program: &IrProgram) -> Option<Shadows<'static>> {
    let resolved;
    let items = match value {
        IrExpr::ListConstruct(items) => items,
        IrExpr::CellRead(cell) => {
            if let Some(IrNode::Derived {
                expr: IrExpr::ListConstruct(items),
                ..
            }) = find_node_for_cell(program, *cell)
            {
                resolved = items.clone();
                &resolved
            } else {
                return None;
            }
        }
        _ => return None,
    };

    let mut shadow_vec = Vec::new();

    for item in items {
        let reconstructed;
        let fields: &[(String, IrExpr)] = match item {
            IrExpr::ObjectConstruct(f) => f,
            IrExpr::CellRead(cell) => {
                reconstructed = reconstruct_object_fields(program, *cell);
                &reconstructed
            }
            _ => continue,
        };

        let mut x = 0i32;
        let mut y = 0i32;
        let mut blur = 0u32;
        let mut spread = 0i32;
        let mut color = "rgba(0,0,0,0.2)".to_string();
        let mut inset = false;

        for (name, val) in fields {
            match name.as_str() {
                "x" => {
                    if let IrExpr::Constant(IrValue::Number(n)) = val {
                        x = *n as i32;
                    }
                }
                "y" => {
                    if let IrExpr::Constant(IrValue::Number(n)) = val {
                        y = *n as i32;
                    }
                }
                "blur" => {
                    if let IrExpr::Constant(IrValue::Number(n)) = val {
                        blur = *n as u32;
                    }
                }
                "spread" => {
                    if let IrExpr::Constant(IrValue::Number(n)) = val {
                        spread = *n as i32;
                    }
                }
                "color" => {
                    color = resolve_color_full(val).unwrap_or(color);
                }
                "direction" => {
                    if let IrExpr::Constant(IrValue::Tag(t)) = val {
                        if t == "Inwards" {
                            inset = true;
                        }
                    }
                }
                _ => {}
            }
        }

        let mut shadow = Shadow::new()
            .x(x)
            .y(y)
            .blur(blur)
            .spread(spread)
            .color(color);
        if inset {
            shadow = shadow.inner();
        }
        shadow_vec.push(shadow);
    }
    if shadow_vec.is_empty() {
        None
    } else {
        Some(Shadows::new(shadow_vec))
    }
}

/// Build a typed Transform style.
fn build_transform(value: &IrExpr, program: &IrProgram) -> Option<Transform> {
    let reconstructed;
    let fields: &[(String, IrExpr)] = match value {
        IrExpr::ObjectConstruct(fields) => fields,
        IrExpr::CellRead(cell) => {
            reconstructed = reconstruct_object_fields(program, *cell);
            &reconstructed
        }
        _ => return None,
    };

    let mut transform = Transform::new();
    let mut has_any = false;

    for (name, val) in fields {
        // Helper: extract f64 from Constant or resolve through CellRead chain.
        let resolve_number = |v: &IrExpr| -> Option<f64> {
            match v {
                IrExpr::Constant(IrValue::Number(n)) => Some(*n),
                _ => resolve_expr_constant(program, v, 0).and_then(|c| {
                    if let ConstValue::Number(n) = c {
                        Some(n)
                    } else {
                        None
                    }
                }),
            }
        };
        match name.as_str() {
            "rotate" => {
                if let Some(n) = resolve_number(val) {
                    transform = transform.rotate(n as i32);
                    has_any = true;
                }
            }
            "scale" => {
                if let Some(n) = resolve_number(val) {
                    transform = transform.scale(n);
                    has_any = true;
                }
            }
            "move_right" => {
                if let Some(n) = resolve_number(val) {
                    transform = transform.move_right(n);
                    has_any = true;
                }
            }
            "move_down" => {
                if let Some(n) = resolve_number(val) {
                    transform = transform.move_down(n);
                    has_any = true;
                }
            }
            "move_left" => {
                if let Some(n) = resolve_number(val) {
                    transform = transform.move_left(n);
                    has_any = true;
                }
            }
            "move_up" => {
                if let Some(n) = resolve_number(val) {
                    transform = transform.move_up(n);
                    has_any = true;
                }
            }
            _ => {}
        }
    }
    if has_any { Some(transform) } else { None }
}

/// Apply typed Zoon styles to a Styleable element.
/// Returns the element with all typed styles applied.
/// Reactive styles (color signals, strikethrough signals, reactive background URLs,
/// outline) and raw CSS exceptions (line-height, font-smoothing) are applied
/// via apply_raw_css in update_raw_el.
fn apply_typed_styles<T: Styleable<'static>>(
    mut el: T,
    style: &IrExpr,
    program: &IrProgram,
    is_row: bool,
) -> T {
    let mut reconstructed_buf = Vec::new();
    let fields = extract_style_fields_vec(style, program, &mut reconstructed_buf);

    for (name, value) in fields {
        match name.as_str() {
            "width" => {
                if let Some(w) = build_width(value) {
                    el = el.s(w);
                }
            }
            "height" => {
                if let Some(h) = build_height(value) {
                    el = el.s(h);
                }
            }
            "size" => {
                if let IrExpr::Constant(IrValue::Number(n)) = value {
                    el = el.s(Width::exact(*n as u32));
                    el = el.s(Height::exact(*n as u32));
                }
            }
            "padding" => {
                if let Some(p) = build_padding(value) {
                    el = el.s(p);
                }
            }
            "font" => {
                if let Some(f) = build_font_static(value, program) {
                    el = el.s(f);
                }
            }
            "background" => {
                if let Some(bg) = build_background_static(value, program) {
                    el = el.s(bg);
                }
            }
            "rounded_corners" => {
                if let Some(rc) = build_rounded_corners(value) {
                    el = el.s(rc);
                }
            }
            "borders" => {
                if let Some(b) = build_borders(value) {
                    el = el.s(b);
                }
            }
            "shadows" => {
                if let Some(s) = build_shadows(value, program) {
                    el = el.s(s);
                }
            }
            "transform" => {
                if let Some(t) = build_transform(value, program) {
                    el = el.s(t);
                }
            }
            "visible" => {
                match value {
                    IrExpr::Constant(IrValue::Bool(b)) => {
                        el = el.s(Visible::new(*b));
                    }
                    IrExpr::Constant(IrValue::Tag(t)) => {
                        el = el.s(Visible::new(t != "False"));
                    }
                    _ => {
                        // Reactive visibility handled by apply_raw_css
                    }
                }
            }
            // align, outline, line_height, font_smoothing, font reactive parts →
            // handled by apply_raw_css in update_raw_el
            _ => {}
        }
    }
    el
}

/// Apply the raw CSS parts that typed Zoon API can't handle:
/// - align (direction-aware flex alignment)
/// - line-height (unitless multiplier)
/// - font-smoothing (vendor prefixed)
/// - outline (reactive WHEN/WHILE signals)
/// - font reactive color/strikethrough
/// - reactive background URL
/// - font-family (pre-formatted CSS)
fn apply_raw_css<T: RawEl>(
    mut el: T,
    style: &IrExpr,
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    item_ctx: Option<&ItemContext>,
    is_row: bool,
) -> T
where
    T::DomElement: AsRef<web_sys::HtmlElement>,
{
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
            "align" => {
                el = apply_align(el, value, program, is_row);
            }
            "line_height" => {
                if let IrExpr::Constant(IrValue::Number(n)) = value {
                    el = el.style("line-height", &format!("{}", n));
                }
            }
            "font_smoothing" => {
                if let IrExpr::Constant(IrValue::Tag(t)) = value {
                    if t == "Antialiased" {
                        el = el.after_insert(|dom_el| {
                            let element: &web_sys::HtmlElement = dom_el.as_ref();
                            let style = element.style();
                            let _ = style.set_property("-webkit-font-smoothing", "antialiased");
                            let _ = style.set_property("-moz-osx-font-smoothing", "grayscale");
                        });
                    }
                }
            }
            "outline" => {
                el = apply_outline(el, value, program, instance);
            }
            "font" => {
                // Apply reactive font parts (color signal, strikethrough signal).
                el = apply_font_reactive(el, value, program, instance, item_ctx);
            }
            "background" => {
                // Apply reactive background URL and color.
                el = apply_background_reactive(el, value, program, instance, item_ctx);
            }
            "visible" => {
                // Reactive visibility — static handled in apply_typed_styles.
                if !matches!(value, IrExpr::Constant(_)) {
                    el = apply_reactive_visible(el, value, program, instance, item_ctx);
                }
            }
            "width" => {
                // Reactive width — static handled in apply_typed_styles.
                if let IrExpr::CellRead(cell) = value {
                    let store = instance.cell_store.clone();
                    let cell_id = cell.0;
                    el = el.style_signal(
                        "width",
                        store
                            .get_cell_signal(cell_id)
                            .map(|v| format!("{}px", v)),
                    );
                }
            }
            "height" => {
                // Reactive height — static handled in apply_typed_styles.
                if let IrExpr::CellRead(cell) = value {
                    let store = instance.cell_store.clone();
                    let cell_id = cell.0;
                    el = el.style_signal(
                        "height",
                        store
                            .get_cell_signal(cell_id)
                            .map(|v| format!("{}px", v)),
                    );
                }
            }
            _ => {}
        }
    }
    el
}

/// Apply physical CSS properties (material, depth, rounded_corners, etc.) for
/// Scene/Element/block and Scene/Element/text elements.
/// These properties exist alongside standard style properties in the same style object.
fn apply_physical_css<T: RawEl>(
    mut el: T,
    style: &IrExpr,
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    item_ctx: Option<&ItemContext>,
) -> T {
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
            "material" => {
                el = apply_physical_material(el, value, program, instance, item_ctx);
            }
            "depth" => {
                el = apply_physical_depth(el, value, program, instance, item_ctx);
            }
            "rounded_corners" => {
                el = apply_physical_rounded_corners(el, value);
            }
            "spring_range" => {
                el = apply_physical_spring_range(el, value, program);
            }
            "rotate" => {
                if let IrExpr::Constant(IrValue::Number(deg)) = value {
                    el = el.style("transform", &format!("rotate({}deg)", deg));
                }
            }
            "size" => {
                // size: [row: N, column: N] → width/height
                let sub_reconstructed;
                let sub_fields: &[(String, IrExpr)] = match value {
                    IrExpr::ObjectConstruct(f) => f,
                    IrExpr::CellRead(c) => {
                        sub_reconstructed = reconstruct_object_fields(program, *c);
                        &sub_reconstructed
                    }
                    _ => continue,
                };
                for (dim, val) in sub_fields {
                    if let IrExpr::Constant(IrValue::Number(n)) = val {
                        match dim.as_str() {
                            "row" => el = el.style("width", &format!("{}px", n)),
                            "column" => el = el.style("height", &format!("{}px", n)),
                            _ => {}
                        }
                    }
                }
            }
            "relief" => {
                el = apply_physical_relief(el, value, program);
            }
            _ => {}
        }
    }
    el
}

/// Apply material properties (color → background-color, glow → box-shadow,
/// transparency → opacity/backdrop-filter, gloss → background-image).
fn apply_physical_material<T: RawEl>(
    mut el: T,
    value: &IrExpr,
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    item_ctx: Option<&ItemContext>,
) -> T {
    let reconstructed;
    let fields: &[(String, IrExpr)] = match value {
        IrExpr::ObjectConstruct(f) => f,
        IrExpr::CellRead(c) => {
            reconstructed = reconstruct_object_fields(program, *c);
            &reconstructed
        }
        _ => return el,
    };
    for (name, val) in fields {
        match name.as_str() {
            "color" => {
                if let Some(css) = resolve_color_full(val) {
                    el = el.style("background-color", &css);
                } else {
                    el = apply_reactive_color(
                        el,
                        "background-color",
                        val,
                        program,
                        instance,
                        item_ctx,
                    );
                }
            }
            "glow" => {
                el = apply_physical_glow(el, val, program, instance, item_ctx);
            }
            "transparency" => {
                if let IrExpr::Constant(IrValue::Number(n)) = val {
                    let opacity = 1.0 - n.clamp(0.0, 1.0);
                    el = el.style("opacity", &format!("{:.2}", opacity));
                    el = el.style("backdrop-filter", "blur(12px)");
                }
            }
            "gloss" => {
                if let IrExpr::Constant(IrValue::Number(n)) = val {
                    let gloss = n.clamp(0.0, 1.0);
                    if gloss > 0.0 {
                        let alpha = gloss * 0.25;
                        el = el.style(
                            "background-image",
                            &format!(
                                "linear-gradient(145deg,rgba(255,255,255,{alpha:.2}) 0%,transparent 50%,rgba(0,0,0,{:.2}) 100%)",
                                alpha * 0.3
                            ),
                        );
                    }
                }
            }
            _ => {}
        }
    }
    el
}

/// Apply depth as box-shadow (elevation-based shadow).
fn apply_physical_depth<T: RawEl>(
    el: T,
    value: &IrExpr,
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    item_ctx: Option<&ItemContext>,
) -> T {
    match value {
        IrExpr::Constant(IrValue::Number(depth)) => {
            if *depth > 0.0 {
                let dx = depth * 0.5;
                let dy = depth * 1.5;
                let blur = depth * 3.0;
                let opacity = (depth * 0.06).min(0.4);
                let amb_blur = blur * 2.0;
                let amb_opacity = opacity * 0.4;
                el.style(
                    "box-shadow",
                    &format!(
                        "{dx:.1}px {dy:.1}px {blur:.1}px rgba(0,0,0,{opacity:.2}), 0px {:.1}px {amb_blur:.1}px rgba(0,0,0,{amb_opacity:.2})",
                        dy * 0.5
                    ),
                )
            } else {
                el
            }
        }
        IrExpr::CellRead(cell) => {
            let store = instance.cell_store.clone();
            let cell_id = cell.0;
            let is_template = item_ctx.map_or(false, |ctx| {
                cell_id >= ctx.template_cell_range.0 && cell_id < ctx.template_cell_range.1
            });
            if is_template {
                if let Some(ctx) = item_ctx {
                    if let Some(ref ics) = instance.item_cell_store {
                        let signal = ics.get_signal(ctx.item_idx, cell_id);
                        return el.style_signal(
                            "box-shadow",
                            signal.map(move |depth| {
                                if depth > 0.0 {
                                    let dx = depth * 0.5;
                                    let dy = depth * 1.5;
                                    let blur = depth * 3.0;
                                    let opacity = (depth * 0.06).min(0.4);
                                    let amb_blur = blur * 2.0;
                                    let amb_opacity = opacity * 0.4;
                                    Some(format!(
                                        "{dx:.1}px {dy:.1}px {blur:.1}px rgba(0,0,0,{opacity:.2}), 0px {:.1}px {amb_blur:.1}px rgba(0,0,0,{amb_opacity:.2})",
                                        dy * 0.5
                                    ))
                                } else {
                                    Some("none".to_string())
                                }
                            }),
                        );
                    }
                }
            }
            el.style_signal(
                "box-shadow",
                store.get_cell_signal(cell_id).map(move |depth| {
                    if depth > 0.0 {
                        let dx = depth * 0.5;
                        let dy = depth * 1.5;
                        let blur = depth * 3.0;
                        let opacity = (depth * 0.06).min(0.4);
                        let amb_blur = blur * 2.0;
                        let amb_opacity = opacity * 0.4;
                        Some(format!(
                            "{dx:.1}px {dy:.1}px {blur:.1}px rgba(0,0,0,{opacity:.2}), 0px {:.1}px {amb_blur:.1}px rgba(0,0,0,{amb_opacity:.2})",
                            dy * 0.5
                        ))
                    } else {
                        Some("none".to_string())
                    }
                }),
            )
        }
        _ => el,
    }
}

/// Apply glow as a colored box-shadow.
fn apply_physical_glow<T: RawEl>(
    mut el: T,
    value: &IrExpr,
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    item_ctx: Option<&ItemContext>,
) -> T {
    // glow: None → no glow
    if let IrExpr::Constant(IrValue::Tag(t)) = value {
        if t == "None" {
            return el;
        }
    }
    let reconstructed;
    let fields: &[(String, IrExpr)] = match value {
        IrExpr::ObjectConstruct(f) => f,
        IrExpr::CellRead(c) => {
            reconstructed = reconstruct_object_fields(program, *c);
            &reconstructed
        }
        _ => return el,
    };
    let mut color_css = None;
    let mut intensity = 0.1_f64;
    let mut color_expr = None;
    for (name, val) in fields {
        match name.as_str() {
            "color" => {
                if let Some(css) = resolve_color_full(val) {
                    color_css = Some(css);
                } else {
                    color_expr = Some(val);
                }
            }
            "intensity" => {
                if let IrExpr::Constant(IrValue::Number(n)) = val {
                    intensity = *n;
                }
            }
            _ => {}
        }
    }
    if let Some(css) = color_css {
        let blur = intensity * 40.0;
        let spread = intensity * 10.0;
        // Merge with existing box-shadow via a second shadow value
        el = el.style(
            "box-shadow",
            &format!("0 0 {blur:.1}px {spread:.1}px {css}"),
        );
    } else if let Some(expr) = color_expr {
        // Reactive glow color
        el = apply_reactive_color(el, "box-shadow", expr, program, instance, item_ctx);
    }
    el
}

/// Apply rounded_corners as border-radius.
fn apply_physical_rounded_corners<T: RawEl>(el: T, value: &IrExpr) -> T {
    match value {
        IrExpr::Constant(IrValue::Number(n)) => el.style("border-radius", &format!("{}px", n)),
        IrExpr::Constant(IrValue::Tag(t)) if t == "Fully" => {
            el.style("border-radius", "9999px")
        }
        _ => el,
    }
}

/// Apply spring_range as CSS transition.
fn apply_physical_spring_range<T: RawEl>(
    el: T,
    value: &IrExpr,
    program: &IrProgram,
) -> T {
    match value {
        IrExpr::Constant(IrValue::Number(n)) => {
            let duration = (n * 0.15).clamp(0.05, 0.8);
            el.style(
                "transition",
                &format!("all {duration:.2}s cubic-bezier(0.34,1.56,0.64,1)"),
            )
        }
        IrExpr::ObjectConstruct(fields) => {
            let extend = fields
                .iter()
                .find(|(n, _)| n == "extend")
                .and_then(|(_, v)| match v {
                    IrExpr::Constant(IrValue::Number(n)) => Some(*n),
                    _ => None,
                })
                .unwrap_or(0.0);
            let compress = fields
                .iter()
                .find(|(n, _)| n == "compress")
                .and_then(|(_, v)| match v {
                    IrExpr::Constant(IrValue::Number(n)) => Some(*n),
                    _ => None,
                })
                .unwrap_or(0.0);
            let range = extend.max(compress);
            if range > 0.0 {
                let duration = (range * 0.04).clamp(0.08, 0.5);
                el.style(
                    "transition",
                    &format!("all {duration:.2}s cubic-bezier(0.34,1.56,0.64,1)"),
                )
            } else {
                el
            }
        }
        IrExpr::CellRead(cell) => {
            let fields = reconstruct_object_fields(program, *cell);
            let extend = fields
                .iter()
                .find(|(n, _)| n == "extend")
                .and_then(|(_, v)| match v {
                    IrExpr::Constant(IrValue::Number(n)) => Some(*n),
                    _ => None,
                })
                .unwrap_or(0.0);
            let compress = fields
                .iter()
                .find(|(n, _)| n == "compress")
                .and_then(|(_, v)| match v {
                    IrExpr::Constant(IrValue::Number(n)) => Some(*n),
                    _ => None,
                })
                .unwrap_or(0.0);
            let range = extend.max(compress);
            if range > 0.0 {
                let duration = (range * 0.04).clamp(0.08, 0.5);
                el.style(
                    "transition",
                    &format!("all {duration:.2}s cubic-bezier(0.34,1.56,0.64,1)"),
                )
            } else {
                el
            }
        }
        _ => el,
    }
}

/// Apply relief as text shadow or inset shadow.
fn apply_physical_relief<T: RawEl>(el: T, value: &IrExpr, _program: &IrProgram) -> T {
    match value {
        IrExpr::Constant(IrValue::Tag(t)) if t == "Raised" => el.style(
            "text-shadow",
            "0 1px 1px rgba(255,255,255,0.3), 0 -1px 1px rgba(0,0,0,0.2)",
        ),
        IrExpr::Constant(IrValue::Tag(t)) if t == "Sunken" => el.style(
            "text-shadow",
            "0 -1px 1px rgba(255,255,255,0.3), 0 1px 1px rgba(0,0,0,0.2)",
        ),
        IrExpr::TaggedObject { tag, fields } if tag == "Carved" => {
            let wall = fields
                .iter()
                .find(|(n, _)| n == "wall")
                .and_then(|(_, v)| match v {
                    IrExpr::Constant(IrValue::Number(n)) => Some(*n),
                    _ => None,
                })
                .unwrap_or(1.0);
            el.style(
                "text-shadow",
                &format!(
                    "0 {wall}px 0 rgba(255,255,255,0.3), 0 -{wall}px 0 rgba(0,0,0,0.2)"
                ),
            )
        }
        _ => el,
    }
}

/// Apply reactive font parts (color signal, strikethrough signal) — for use in update_raw_el.
fn apply_font_reactive<T: RawEl>(
    mut el: T,
    value: &IrExpr,
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    item_ctx: Option<&ItemContext>,
) -> T {
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
                // Only apply reactive color — static color is in build_font_static.
                if resolve_color(val).is_none() {
                    el = apply_reactive_color(el, "color", val, program, instance, item_ctx);
                }
            }
            "family" => {
                // Font family uses pre-formatted CSS strings that don't map
                // cleanly to FontFamily items (would double-quote).
                if let Some(family) = resolve_font_family(val, program) {
                    el = el.style("font-family", &family);
                }
            }
            "line" => {
                el = apply_font_line(el, val, program, instance, item_ctx);
            }
            _ => {}
        }
    }
    el
}

/// Apply reactive background URL — for use in update_raw_el.
fn apply_background_reactive<T: RawEl>(
    mut el: T,
    value: &IrExpr,
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    item_ctx: Option<&ItemContext>,
) -> T
where
    T::DomElement: AsRef<web_sys::HtmlElement>,
{
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
                // Only apply reactive color — static color is in build_background_static.
                if resolve_color(val).is_none() {
                    el = apply_reactive_color(
                        el,
                        "background-color",
                        val,
                        program,
                        instance,
                        item_ctx,
                    );
                }
            }
            "url" => {
                let url = eval_static_text(val);
                let url2 = if url.is_empty() {
                    resolve_static_text(program, val)
                } else {
                    url
                };
                if url2.is_empty() {
                    // No static URL — must be reactive.
                    el = apply_reactive_background_url(el, val, program, instance, item_ctx);
                }
            }
            _ => {}
        }
    }
    el
}

/// Follow Derived(CellRead) and PipeThrough chains to find the underlying cell
/// that has `cell_field_cells` entries. Returns the original cell if no chain is found.
fn resolve_to_object_store(program: &IrProgram, cell: CellId) -> CellId {
    if program.cell_field_cells.contains_key(&cell) {
        return cell;
    }
    if let Some(node) = find_node_for_cell(program, cell) {
        match node {
            IrNode::Derived {
                expr: IrExpr::CellRead(src),
                ..
            } => return resolve_to_object_store(program, *src),
            IrNode::PipeThrough { source, .. } => {
                return resolve_to_object_store(program, *source)
            }
            _ => {}
        }
    }
    cell
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
    // Follow Derived(CellRead) and PipeThrough chains to find cell_field_cells.
    let resolved_cell = resolve_to_object_store(program, cell);
    if let Some(field_map) = program.cell_field_cells.get(&resolved_cell) {
        for (name, field_cell) in field_map {
            if let Some(node) = find_node_for_cell(program, *field_cell) {
                match node {
                    IrNode::Derived { expr, .. } => {
                        // Check if this field is itself a namespace cell (nested object
                        // with sub-fields in cell_field_cells). This occurs for:
                        // - Object-store namespaces (Derived(Void) from lower_object_store)
                        // - Spread alias cells (Derived(CellRead) from spread handling)
                        // - WHEN-distributed namespaces (Derived(Void) from distribution)
                        if program.cell_field_cells.contains_key(field_cell) {
                            let inner = reconstruct_object_fields(program, *field_cell);
                            fields.push((name.clone(), IrExpr::ObjectConstruct(inner)));
                        } else {
                            fields.push((name.clone(), expr.clone()));
                        }
                    }
                    // Non-Derived nodes (When, While, Hold, etc.) — check for
                    // cell_field_cells first (may be namespace), otherwise expose
                    // as CellRead for reactive processing.
                    _ => {
                        if program.cell_field_cells.contains_key(field_cell) {
                            let inner = reconstruct_object_fields(program, *field_cell);
                            fields.push((name.clone(), IrExpr::ObjectConstruct(inner)));
                        } else {
                            fields.push((name.clone(), IrExpr::CellRead(*field_cell)));
                        }
                    }
                }
            }
        }
    }
    fields
}

/// Apply font line properties (strikethrough, underline).
fn apply_font_line<T: RawEl>(
    mut el: T,
    value: &IrExpr,
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    item_ctx: Option<&ItemContext>,
) -> T {
    // value is [strikethrough: Bool/CellRead]
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
        if name == "strikethrough" {
            match val {
                IrExpr::Constant(IrValue::Bool(true)) => {
                    el = el.style("text-decoration", "line-through");
                }
                IrExpr::Constant(IrValue::Bool(false)) => {}
                IrExpr::CellRead(cell) => {
                    // Reactive strikethrough — use style_signal.
                    if let Some(ctx) = item_ctx {
                        if ctx.is_template_cell(*cell) {
                            if let Some(ref ics) = instance.item_cell_store {
                                // Per-item cell — use item cell store signal.
                                let cell_id = cell.0;
                                let item_idx = ctx.item_idx;
                                el = el.style_signal(
                                    "text-decoration",
                                    ics.get_signal(item_idx, cell_id).map(|v| {
                                        if v != 0.0 {
                                            Some("line-through")
                                        } else {
                                            Some("none")
                                        }
                                    }),
                                );
                                continue;
                            }
                        }
                    }
                    // Global cell — use cell store.
                    let store = instance.cell_store.clone();
                    let cell_id = cell.0;
                    el = el.style_signal(
                        "text-decoration",
                        store.get_cell_signal(cell_id).map(|v| {
                            if v != 0.0 {
                                Some("line-through")
                            } else {
                                Some("none")
                            }
                        }),
                    );
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
            if let Some(IrNode::Derived {
                expr: IrExpr::ListConstruct(items),
                ..
            }) = find_node_for_cell(program, *cell)
            {
                items.clone()
            } else {
                return None;
            }
        }
        _ => return None,
    };

    let families: Vec<String> = items
        .iter()
        .filter_map(|item| match item {
            IrExpr::Constant(IrValue::Text(t)) => Some(format!("\"{}\"", t)),
            IrExpr::TextConcat(parts) => {
                let text: String = parts
                    .iter()
                    .map(|p| match p {
                        TextSegment::Literal(s) => s.clone(),
                        _ => String::new(),
                    })
                    .collect();
                if text.is_empty() {
                    None
                } else {
                    Some(format!("\"{}\"", text))
                }
            }
            IrExpr::Constant(IrValue::Tag(t)) => match t.as_str() {
                "SansSerif" => Some("sans-serif".to_string()),
                "Serif" => Some("serif".to_string()),
                "Monospace" => Some("monospace".to_string()),
                _ => Some(t.clone()),
            },
            IrExpr::CellRead(cell) => {
                if let Some(node) = find_node_for_cell(program, *cell) {
                    match node {
                        IrNode::TextInterpolation { parts, .. }
                        | IrNode::Derived {
                            expr: IrExpr::TextConcat(parts),
                            ..
                        } => {
                            let text: String = parts
                                .iter()
                                .map(|p| match p {
                                    TextSegment::Literal(s) => s.clone(),
                                    _ => String::new(),
                                })
                                .collect();
                            if text.is_empty() {
                                None
                            } else {
                                Some(format!("\"{}\"", text))
                            }
                        }
                        IrNode::Derived {
                            expr: IrExpr::Constant(IrValue::Text(t)),
                            ..
                        } => Some(format!("\"{}\"", t)),
                        IrNode::Derived {
                            expr: IrExpr::Constant(IrValue::Tag(t)),
                            ..
                        } => match t.as_str() {
                            "SansSerif" => Some("sans-serif".to_string()),
                            "Serif" => Some("serif".to_string()),
                            "Monospace" => Some("monospace".to_string()),
                            _ => Some(t.clone()),
                        },
                        _ => None,
                    }
                } else {
                    None
                }
            }
            _ => None,
        })
        .collect();

    if families.is_empty() {
        None
    } else {
        Some(families.join(", "))
    }
}

/// Apply reactive visibility via style_signal. Watches a cell and maps
/// truthy (non-zero) to "visible", falsy (zero) to "hidden".
fn apply_reactive_visible<T: RawEl>(
    el: T,
    value: &IrExpr,
    _program: &IrProgram,
    instance: &Rc<WasmInstance>,
    item_ctx: Option<&ItemContext>,
) -> T {
    if let IrExpr::CellRead(cell) = value {
        let cell_id = cell.0;
        // Per-item template cell path.
        if let Some(ctx) = item_ctx {
            if ctx.is_template_cell(*cell) {
                if let Some(ref ics) = instance.item_cell_store {
                    let item_idx = ctx.item_idx;
                    return el.style_signal(
                        "visibility",
                        ics.get_signal(item_idx, cell_id)
                            .map(|v| if v != 0.0 { "visible" } else { "hidden" }),
                    );
                }
            }
        }
        // Global cell path.
        let store = instance.cell_store.clone();
        return el.style_signal(
            "visibility",
            store
                .get_cell_signal(cell_id)
                .map(|v| if v != 0.0 { "visible" } else { "hidden" }),
        );
    }
    el
}

/// Set up a reactive background-image from a WHEN/WHILE expression.
/// Collects text for each arm statically, watches the source cell, and
/// switches the CSS property when the source value changes.
fn apply_reactive_background_url<T: RawEl>(
    mut el: T,
    url_expr: &IrExpr,
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    item_ctx: Option<&ItemContext>,
) -> T
where
    T::DomElement: AsRef<web_sys::HtmlElement>,
{
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
                        el = el.after_insert(move |dom_el| {
                            let element: web_sys::HtmlElement =
                                AsRef::<web_sys::HtmlElement>::as_ref(&dom_el).clone();
                            let handle = Task::start_droppable(
                                ics_clone.get_signal(item_idx, source_id).for_each_sync(
                                    move |val| {
                                        for (pat_val, url) in &arm_urls {
                                            if val == *pat_val {
                                                let _ = element.style().set_property(
                                                    "background-image",
                                                    &format!("url({})", url),
                                                );
                                                break;
                                            }
                                        }
                                    },
                                ),
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
                el = el.after_insert(move |dom_el| {
                    let element: web_sys::HtmlElement =
                        AsRef::<web_sys::HtmlElement>::as_ref(&dom_el).clone();
                    let handle = Task::start_droppable(
                        store.get_cell_signal(source_id).for_each_sync(move |val| {
                            for (pat_val, url) in &arm_urls {
                                if val == *pat_val {
                                    let _ = element
                                        .style()
                                        .set_property("background-image", &format!("url({})", url));
                                    break;
                                }
                            }
                        }),
                    );
                    std::mem::forget(handle);
                });
            }
        }
    }
    el
}

/// Apply align properties (maps to flex alignment).
///
/// Boon's `align: [row: X]` = horizontal, `[column: X]` = vertical.
/// CSS flexbox mapping depends on flex-direction:
///   Column: horizontal → align-items, vertical → justify-content
///   Row:    horizontal → justify-content, vertical → align-items
fn apply_align<T: RawEl>(
    mut el: T,
    value: &IrExpr,
    program: &IrProgram,
    is_row_direction: bool,
) -> T {
    // The align value may be ObjectConstruct (direct) or CellRead (when the
    // style object went through lower_object_store). Reconstruct if needed.
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
        // Resolve CellRead values through the IR chain to find the tag constant.
        // Function parameters (like `align: End`) become CellRead cells after inlining.
        let tag = match val {
            IrExpr::Constant(IrValue::Tag(t)) => Some(t.clone()),
            _ => {
                if let Some(ConstValue::Tag(t)) = resolve_expr_constant(program, val, 0) {
                    Some(t)
                } else {
                    None
                }
            }
        };
        if let Some(t) = tag {
            let css_val = match t.as_str() {
                "Center" => "center",
                "Start" | "Left" | "Top" => "flex-start",
                "End" | "Right" | "Bottom" => "flex-end",
                _ => "flex-start",
            };
            match (name.as_str(), is_row_direction) {
                // Column flex: row=cross(align-items), column=main(justify-content)
                ("row", false) => {
                    el = el.style("align-items", css_val);
                }
                ("column", false) => {
                    el = el.style("justify-content", css_val);
                }
                // Row flex: row=main(justify-content), column=cross(align-items)
                ("row", true) => {
                    el = el.style("justify-content", css_val);
                }
                ("column", true) => {
                    el = el.style("align-items", css_val);
                }
                _ => {}
            }
        }
    }
    el
}

/// Convert an IrPattern to the f64 value the WASM cell would hold when that arm matches.
fn pattern_to_f64(pat: &IrPattern, program: &IrProgram) -> f64 {
    match pat {
        IrPattern::Number(n) => *n,
        IrPattern::Tag(tag) => program
            .tag_table
            .iter()
            .position(|t| t == tag)
            .map(|i| (i + 1) as f64)
            .unwrap_or(0.0),
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
            let color = fields
                .iter()
                .find(|(n, _)| n == "color")
                .and_then(|(_, v)| resolve_color_full(v))
                .unwrap_or_else(|| "currentColor".to_string());
            let side = fields.iter().find(|(n, _)| n == "side").and_then(|(_, v)| {
                if let IrExpr::Constant(IrValue::Tag(t)) = v {
                    Some(t.as_str())
                } else {
                    None
                }
            });
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
                        IrNode::While {
                            source: inner_source,
                            arms: inner_arms,
                            ..
                        }
                        | IrNode::When {
                            source: inner_source,
                            arms: inner_arms,
                            ..
                        } => {
                            collect_outline_arm_css(
                                program,
                                instance,
                                inner_arms,
                                *inner_source,
                                result,
                            );
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

fn apply_outline<T: RawEl>(
    mut el: T,
    value: &IrExpr,
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
) -> T
where
    T::DomElement: AsRef<web_sys::HtmlElement>,
{
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
                        let mut source_cells: Vec<CellId> =
                            all_css.iter().map(|(s, _, _, _)| *s).collect();
                        source_cells.sort_by_key(|c| c.0);
                        source_cells.dedup();

                        // Set initial outline from current cell values.
                        let initial_css = resolve_active_outline(&all_css, &instance.cell_store);
                        if let Some((outline, shadow)) = initial_css {
                            if !outline.is_empty() {
                                el = el.style("outline", &outline);
                            }
                            if !shadow.is_empty() {
                                el = el.style("box-shadow", &shadow);
                            }
                        }

                        // Watch all source cells and update outline reactively.
                        let store = instance.cell_store.clone();
                        let source_cells_clone = source_cells.clone();
                        el = el.after_insert(move |dom_el| {
                            let element: web_sys::HtmlElement =
                                AsRef::<web_sys::HtmlElement>::as_ref(&dom_el).clone();
                            for source_cell in &source_cells_clone {
                                let all_css = all_css.clone();
                                let store2 = store.clone();
                                let el2 = element.clone();
                                let handle = Task::start_droppable(
                                    store.get_cell_signal(source_cell.0).for_each_sync(
                                        move |_val| {
                                            if let Some((outline, shadow)) =
                                                resolve_active_outline(&all_css, &store2)
                                            {
                                                let style = el2.style();
                                                let _ = style.set_property("outline", &outline);
                                                let _ = style.set_property("box-shadow", &shadow);
                                            }
                                        },
                                    ),
                                );
                                std::mem::forget(handle);
                            }
                        });
                    }
                    IrNode::Derived { expr, .. } => {
                        let (outline, shadow) = resolve_outline_css(expr);
                        if !outline.is_empty() {
                            el = el.style("outline", &outline);
                        }
                        if !shadow.is_empty() {
                            el = el.style("box-shadow", &shadow);
                        }
                    }
                    _ => {}
                }
            }
        }
        other => {
            let (outline, shadow) = resolve_outline_css(other);
            if !outline.is_empty() {
                el = el.style("outline", &outline);
            }
            if !shadow.is_empty() {
                el = el.style("box-shadow", &shadow);
            }
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

/// Full color resolver — handles Oklch with all parameters.
fn resolve_color_full(expr: &IrExpr) -> Option<String> {
    match expr {
        IrExpr::Constant(IrValue::Tag(tag)) => Some(tag.to_lowercase()),
        IrExpr::TaggedObject { tag, fields } if tag == "Oklch" => {
            let mut lightness = 0.0;
            let mut chroma = 0.0;
            let mut hue = 0.0;
            let mut alpha = 1.0;
            for (name, value) in fields {
                match value {
                    IrExpr::Constant(IrValue::Number(n)) => match name.as_str() {
                        "lightness" => lightness = *n,
                        "chroma" => chroma = *n,
                        "hue" => hue = *n,
                        "alpha" => alpha = *n,
                        _ => {}
                    },
                    // If any field is reactive (CellRead, etc.), this color
                    // can't be resolved statically — return None so the caller
                    // falls through to apply_reactive_color.
                    _ => return None,
                }
            }
            if alpha < 1.0 {
                Some(format!(
                    "oklch({} {} {} / {})",
                    lightness, chroma, hue, alpha
                ))
            } else {
                Some(format!("oklch({} {} {})", lightness, chroma, hue))
            }
        }
        _ => None,
    }
}

/// Apply a reactive Oklch color as a style_signal. Handles Oklch with CellRead fields.
fn apply_reactive_color<T: RawEl>(
    el: T,
    css_prop: &'static str,
    expr: &IrExpr,
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    item_ctx: Option<&ItemContext>,
) -> T {
    // Case 1: TaggedObject or ObjectConstruct with reactive CellRead fields for Oklch components.
    // TaggedObject occurs when the lowerer emits Oklch directly.
    // ObjectConstruct occurs when reconstruct_object_fields expands a distributed WHEN namespace
    // (cell_field_cells with lightness/chroma/hue sub-cells) into an inline ObjectConstruct.
    let oklch_fields: Option<&[(String, IrExpr)]> = match expr {
        IrExpr::TaggedObject { tag, fields } if tag == "Oklch" => Some(fields),
        IrExpr::ObjectConstruct(fields) => {
            let has_oklch = fields.iter().any(|(n, _)| n == "lightness" || n == "chroma" || n == "hue");
            if has_oklch { Some(fields) } else { None }
        }
        _ => None,
    };
    if let Some(fields) = oklch_fields {
        let mut lightness_cell = None;
        let mut chroma_cell = None;
        let mut hue_cell = None;
        let mut lightness_default = 0.0f64;
        let mut chroma_default = 0.0f64;
        let mut hue_default = 0.0f64;
        let mut alpha_val = 1.0f64;
        for (name, value) in fields {
            match name.as_str() {
                "lightness" => match value {
                    IrExpr::Constant(IrValue::Number(n)) => lightness_default = *n,
                    IrExpr::CellRead(cell) => lightness_cell = Some(cell.0),
                    _ => {}
                },
                "chroma" => match value {
                    IrExpr::Constant(IrValue::Number(n)) => chroma_default = *n,
                    IrExpr::CellRead(cell) => chroma_cell = Some(cell.0),
                    _ => {}
                },
                "hue" => match value {
                    IrExpr::Constant(IrValue::Number(n)) => hue_default = *n,
                    IrExpr::CellRead(cell) => hue_cell = Some(cell.0),
                    _ => {}
                },
                "alpha" => {
                    if let IrExpr::Constant(IrValue::Number(n)) = value {
                        alpha_val = *n;
                    }
                }
                _ => {}
            }
        }
        // Pick any reactive cell as signal driver, read the rest in the closure.
        let driver_cell = lightness_cell.or(chroma_cell).or(hue_cell);
        if let Some(cell_id) = driver_cell {
            // Check if driver cell is in the template range (per-item list cells).
            let is_template = item_ctx.map_or(false, |ctx| {
                cell_id >= ctx.template_cell_range.0 && cell_id < ctx.template_cell_range.1
            });
            if is_template {
                if let Some(ctx) = item_ctx {
                    if let Some(ref ics) = instance.item_cell_store {
                        let signal = ics.get_signal(ctx.item_idx, cell_id);
                        let item_store = instance.item_cell_store.clone().unwrap();
                        let global_store = instance.cell_store.clone();
                        let item_idx = ctx.item_idx;
                        return el.style_signal(
                            css_prop,
                            signal.map(move |_| {
                                let l = lightness_cell.map_or(lightness_default, |c| {
                                    let v = item_store.get_value(item_idx, c);
                                    if v.is_nan() { global_store.get_cell_value(c) } else { v }
                                });
                                let c = chroma_cell.map_or(chroma_default, |c2| {
                                    let v = item_store.get_value(item_idx, c2);
                                    if v.is_nan() { global_store.get_cell_value(c2) } else { v }
                                });
                                let h = hue_cell.map_or(hue_default, |c3| {
                                    let v = item_store.get_value(item_idx, c3);
                                    if v.is_nan() { global_store.get_cell_value(c3) } else { v }
                                });
                                if l.is_nan() || c.is_nan() || h.is_nan() {
                                    return None;
                                }
                                if alpha_val < 1.0 {
                                    Some(format!("oklch({} {} {} / {})", l, c, h, alpha_val))
                                } else {
                                    Some(format!("oklch({} {} {})", l, c, h))
                                }
                            }),
                        );
                    }
                }
            }
            let store = instance.cell_store.clone();
            return el.style_signal(
                css_prop,
                store.get_cell_signal(cell_id).map(move |_| {
                    let l =
                        lightness_cell.map_or(lightness_default, |cid| store.get_cell_value(cid));
                    let c = chroma_cell.map_or(chroma_default, |cid| store.get_cell_value(cid));
                    let h = hue_cell.map_or(hue_default, |hid| store.get_cell_value(hid));
                    if l.is_nan() || c.is_nan() || h.is_nan() {
                        return None;
                    }
                    if alpha_val < 1.0 {
                        Some(format!("oklch({} {} {} / {})", l, c, h, alpha_val))
                    } else {
                        Some(format!("oklch({} {} {})", l, c, h))
                    }
                }),
            );
        }
    }
    // Case 2: CellRead pointing to a PatternMatch node whose arms produce constant Oklch values.
    // This handles: `completed |> WHILE { True => Oklch[lightness: 0.647], False => Oklch[lightness: 0.42] }`
    // The PatternMatch source cell (completed) changes per-item, and each arm body resolves to a static CSS color.
    if let IrExpr::CellRead(cell) = expr {
        if let Some(color_mapping) = resolve_pattern_match_colors(program, *cell) {
            // color_mapping: (source_cell, vec of (pattern_value, css_color), default_css)
            let (source_cell, arms, default_css) = color_mapping;
            let is_template = item_ctx.map_or(false, |ctx| {
                source_cell.0 >= ctx.template_cell_range.0
                    && source_cell.0 < ctx.template_cell_range.1
            });
            if is_template {
                if let Some(ctx) = item_ctx {
                    if let Some(ref ics) = instance.item_cell_store {
                        let signal = ics.get_signal(ctx.item_idx, source_cell.0);
                        return el.style_signal(
                            css_prop,
                            signal.map(move |v| {
                                for (pattern_val, css) in &arms {
                                    if v.to_bits() == pattern_val.to_bits() {
                                        return Some(css.clone());
                                    }
                                }
                                Some(default_css.clone())
                            }),
                        );
                    }
                }
            } else {
                let store = instance.cell_store.clone();
                return el.style_signal(
                    css_prop,
                    store.get_cell_signal(source_cell.0).map(move |v| {
                        for (pattern_val, css) in &arms {
                            if v.to_bits() == pattern_val.to_bits() {
                                return Some(css.clone());
                            }
                        }
                        Some(default_css.clone())
                    }),
                );
            }
        }
    }
    // Case 3: CellRead pointing to an object-store with lightness/chroma/hue sub-fields.
    // This handles distributed WHEN results where Oklch objects are split into per-component cells.
    if let IrExpr::CellRead(cell) = expr {
        let resolved = resolve_to_object_store(program, *cell);
        if let Some(field_map) = program.cell_field_cells.get(&resolved) {
            let has_oklch_fields = field_map.contains_key("lightness")
                || field_map.contains_key("chroma")
                || field_map.contains_key("hue");
            if has_oklch_fields {
                return apply_oklch_from_fields(
                    el, css_prop, field_map, program, instance, item_ctx,
                );
            }
        }
    }
    el
}

/// Apply an Oklch color from distributed object-store fields (lightness, chroma, hue, alpha).
fn apply_oklch_from_fields<T: RawEl>(
    el: T,
    css_prop: &'static str,
    field_map: &std::collections::HashMap<String, CellId>,
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    item_ctx: Option<&ItemContext>,
) -> T {
    let mut lightness_cell: Option<u32> = None;
    let mut chroma_cell: Option<u32> = None;
    let mut hue_cell: Option<u32> = None;
    let mut lightness_default = 0.0f64;
    let mut chroma_default = 0.0f64;
    let mut hue_default = 0.0f64;
    let mut alpha_val = 1.0f64;

    for (name, field_cell) in field_map {
        // Resolve the field's expression to either a constant or a reactive cell.
        let expr = if let Some(node) = find_node_for_cell(program, *field_cell) {
            match node {
                IrNode::Derived { expr, .. } => expr.clone(),
                _ => IrExpr::CellRead(*field_cell),
            }
        } else {
            IrExpr::CellRead(*field_cell)
        };

        match name.as_str() {
            "lightness" => match &expr {
                IrExpr::Constant(IrValue::Number(n)) => lightness_default = *n,
                _ => lightness_cell = Some(field_cell.0),
            },
            "chroma" => match &expr {
                IrExpr::Constant(IrValue::Number(n)) => chroma_default = *n,
                _ => chroma_cell = Some(field_cell.0),
            },
            "hue" => match &expr {
                IrExpr::Constant(IrValue::Number(n)) => hue_default = *n,
                _ => hue_cell = Some(field_cell.0),
            },
            "alpha" => {
                if let IrExpr::Constant(IrValue::Number(n)) = &expr {
                    alpha_val = *n;
                }
            }
            _ => {}
        }
    }

    let driver_cell = lightness_cell.or(chroma_cell).or(hue_cell);
    if let Some(cell_id) = driver_cell {
        let is_template = item_ctx.map_or(false, |ctx| {
            cell_id >= ctx.template_cell_range.0 && cell_id < ctx.template_cell_range.1
        });
        if is_template {
            if let Some(ctx) = item_ctx {
                if let Some(ref ics) = instance.item_cell_store {
                    let signal = ics.get_signal(ctx.item_idx, cell_id);
                    let item_store = instance.item_cell_store.clone().unwrap();
                    let global_store = instance.cell_store.clone();
                    let item_idx = ctx.item_idx;
                    return el.style_signal(
                        css_prop,
                        signal.map(move |_| {
                            // Read from ItemCellStore, fall back to CellStore if NaN.
                            let l = lightness_cell.map_or(lightness_default, |c| {
                                let v = item_store.get_value(item_idx, c);
                                if v.is_nan() { global_store.get_cell_value(c) } else { v }
                            });
                            let c = chroma_cell.map_or(chroma_default, |c2| {
                                let v = item_store.get_value(item_idx, c2);
                                if v.is_nan() { global_store.get_cell_value(c2) } else { v }
                            });
                            let h = hue_cell.map_or(hue_default, |c3| {
                                let v = item_store.get_value(item_idx, c3);
                                if v.is_nan() { global_store.get_cell_value(c3) } else { v }
                            });
                            if l.is_nan() || c.is_nan() || h.is_nan() {
                                return None; // Skip setting style if values aren't ready
                            }
                            if alpha_val < 1.0 {
                                Some(format!("oklch({} {} {} / {})", l, c, h, alpha_val))
                            } else {
                                Some(format!("oklch({} {} {})", l, c, h))
                            }
                        }),
                    );
                }
            }
        }
        let store = instance.cell_store.clone();
        return el.style_signal(
            css_prop,
            store.get_cell_signal(cell_id).map(move |_| {
                let l = lightness_cell.map_or(lightness_default, |cid| store.get_cell_value(cid));
                let c = chroma_cell.map_or(chroma_default, |cid| store.get_cell_value(cid));
                let h = hue_cell.map_or(hue_default, |hid| store.get_cell_value(hid));
                if l.is_nan() || c.is_nan() || h.is_nan() {
                    return None; // Skip setting style if values aren't ready
                }
                if alpha_val < 1.0 {
                    Some(format!("oklch({} {} {} / {})", l, c, h, alpha_val))
                } else {
                    Some(format!("oklch({} {} {})", l, c, h))
                }
            }),
        );
    }
    // All components are static constants.
    if alpha_val < 1.0 {
        el.style(
            css_prop,
            &format!(
                "oklch({} {} {} / {})",
                lightness_default, chroma_default, hue_default, alpha_val
            ),
        )
    } else {
        el.style(
            css_prop,
            &format!(
                "oklch({} {} {})",
                lightness_default, chroma_default, hue_default
            ),
        )
    }
}

/// Try to resolve a CellRead to a PatternMatch with constant Oklch arm bodies.
/// Returns (source_cell, vec of (pattern_f64_value, css_color_string), default_css).
fn resolve_pattern_match_colors(
    program: &IrProgram,
    cell: CellId,
) -> Option<(CellId, Vec<(f64, String)>, String)> {
    // Find the node for this cell.
    for node in &program.nodes {
        // Extract source and arms from When or While nodes.
        let (source, arms) = match node {
            IrNode::When {
                cell: c,
                source,
                arms,
            } if *c == cell => (source, arms),
            IrNode::While {
                cell: c,
                source,
                arms,
                ..
            } if *c == cell => (source, arms),
            _ => continue,
        };
        let mut color_arms: Vec<(f64, String)> = Vec::new();
        let mut default_css = String::new();
        for (pattern, body) in arms {
            let css = resolve_color_full(body)?;
            let pattern_val = pattern_to_f64_opt(pattern, &program.tag_table)?;
            if default_css.is_empty() {
                default_css = css.clone();
            }
            color_arms.push((pattern_val, css));
        }
        if color_arms.is_empty() {
            return None;
        }
        // Resolve source: if it's a CellRead chain, follow it.
        let source_cell = follow_cell_read(program, *source);
        return Some((source_cell, color_arms, default_css));
    }
    None
}

/// Convert an IrPattern to its f64 representation for color mapping.
/// Returns None for patterns that can't be mapped (Text, Binding).
fn pattern_to_f64_opt(pattern: &IrPattern, tag_table: &[String]) -> Option<f64> {
    match pattern {
        IrPattern::Number(n) => Some(*n),
        IrPattern::Tag(t) => tag_table
            .iter()
            .position(|s| s == t)
            .map(|i| (i + 1) as f64),
        IrPattern::Wildcard => Some(f64::NAN),
        _ => None,
    }
}

/// Follow a CellRead chain through Derived { expr: CellRead } nodes.
fn follow_cell_read(program: &IrProgram, cell: CellId) -> CellId {
    for node in &program.nodes {
        if let IrNode::Derived {
            cell: c,
            expr: IrExpr::CellRead(source),
        } = node
        {
            if *c == cell {
                return follow_cell_read(program, *source);
            }
        }
    }
    cell
}
