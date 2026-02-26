//! DOM bridge — creates Zoon UI elements from the IR program and connects
//! them to the WASM runtime instance.

use std::cell::RefCell;
use std::collections::HashSet;
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
                if has_element_arms(program, arms) {
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
            | IrNode::TextTrim { .. }
            | IrNode::TextIsNotEmpty { .. } => build_reactive_text_ctx(instance, ctx, cell),
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
            _ => zoon::Text::new("?").unify(),
        }
    } else {
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
        _ => build_reactive_text(instance, cell),
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
            ..
        } => build_text_input(
            program,
            instance,
            placeholder.as_ref(),
            style,
            links,
            *focus,
            hovered_cell,
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
fn build_label_child(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    label: &IrExpr,
) -> RawElOrText {
    let el = if let Some(segs) = resolve_label_segments(program, label) {
        if segs
            .iter()
            .any(|s| matches!(s, TextSegment::Expr(IrExpr::CellRead(_))))
        {
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

    let label_child = build_label_child(program, instance, label);
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
        apply_raw_css(raw_el, style, program, instance, ctx.item_ctx(), false)
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
            apply_raw_css(raw_el, style, program, instance, ctx.item_ctx(), is_row)
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
                                if let Some((source, arms)) =
                                    resolve_to_conditional(program, *child_cell)
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
                            _ => {}
                        }
                    }
                    children
                }
                IrExpr::CellRead(source) => {
                    collect_stripe_children(program, instance, ctx, *source)
                }
                _ => Vec::new(),
            }
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
                .style("border", "none")
                .style("outline", "none")
                .style("color", "inherit")
                .style("background", "transparent");

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

            raw_el = apply_raw_css(raw_el, style, program, instance, None, false);

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
            apply_raw_css(raw_el, style, program, instance, ctx.item_ctx(), false)
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
            apply_raw_css(raw_el, style, program, instance, ctx.item_ctx(), false)
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
        apply_raw_css(raw_el, style, program, instance, ctx.item_ctx(), false)
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
        .update_raw_el(|raw_el| apply_raw_css(raw_el, style, program, instance, None, false))
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
        .update_raw_el(|raw_el| apply_raw_css(raw_el, style, program, instance, None, false))
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
        apply_raw_css(raw_el, style, program, instance, None, false)
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
            find_matching_arm_idx(&matchers, val, &text).and_then(|idx| {
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
            find_matching_arm_idx(&matchers, val, &text).and_then(|idx| {
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
    // These need to be forwarded to all live items when the global event fires.
    let cross_scope_events =
        collect_cross_scope_events(program, template_cell_range, template_event_range);

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

    // Track which items have been initialized by their stable memory index.
    // On re-renders (e.g. filter changes), existing items keep their HOLD state;
    // only items not yet seen get init_item called.
    let initialized_indices: Rc<RefCell<HashSet<u32>>> = Rc::new(RefCell::new(HashSet::new()));

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
                *live_items_for_closure.borrow_mut() = 0;
                // Reset initialized indices so all items re-initialize on next render.
                initialized_indices.borrow_mut().clear();
                return None;
            }

            *live_items_for_closure.borrow_mut() = item_count as u32;

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

                    // Initialize per-item template cells in WASM memory.
                    // Only init NEW items — existing items keep their HOLD state
                    // (e.g. completed toggle) across re-renders.
                    if !initialized_indices.borrow().contains(&item_idx) {
                        let _ = inst.call_init_item(item_idx);
                        initialized_indices.borrow_mut().insert(item_idx);
                    }

                    // Seed item text from ListStore only when template item text
                    // is still empty. This avoids overwriting restored per-item
                    // state (e.g. edited Todo titles) during reruns.
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

                    // Copy item text to template-local namespace field cells.
                    if let Some(ref ics) = inst.item_cell_store {
                        if let Some(fields) = program.cell_field_cells.get(&item_cell) {
                            let item_text = ics.get_text(item_idx, item_cell.0);
                            if !item_text.is_empty() {
                                for (name, field_cell) in fields {
                                    let in_range = field_cell.0 >= template_cell_range.0
                                        && field_cell.0 < template_cell_range.1;
                                    let is_namespace =
                                        program.cell_field_cells.contains_key(field_cell);
                                    if in_range
                                        && !is_namespace
                                        && (!has_pending_snapshot
                                            || ics.get_text(item_idx, field_cell.0).is_empty())
                                    {
                                        ics.set_text(item_idx, field_cell.0, item_text.clone());
                                    }
                                }
                            }
                        }
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
                .style("border", "none")
                .style("outline", "none")
                .style("color", "inherit")
                .style("background", "transparent");

            raw_el = apply_raw_css(raw_el, style, program, instance, Some(ctx), false);

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
        apply_raw_css(raw_el, style, program, instance, Some(ctx), false)
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
        _ => None,
    }
}

/// Build a typed Height style from an IR dimension expression.
fn build_height(value: &IrExpr) -> Option<Height<'static>> {
    match value {
        IrExpr::Constant(IrValue::Number(n)) => Some(Height::exact(*n as u32)),
        IrExpr::Constant(IrValue::Tag(t)) if t == "Fill" => Some(Height::fill()),
        _ => None,
    }
}

/// Build a typed Padding style from an IR padding object.
fn build_padding(value: &IrExpr) -> Option<Padding<'static>> {
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
    if has_any {
        Some(font)
    } else {
        None
    }
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
    if has_any {
        Some(bg)
    } else {
        None
    }
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
    if has_any {
        Some(transform)
    } else {
        None
    }
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
            _ => {}
        }
    }
    el
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
                        ics.get_signal(item_idx, cell_id).map(|v| {
                            if v != 0.0 {
                                "visible"
                            } else {
                                "hidden"
                            }
                        }),
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
    // Case 1: TaggedObject with reactive CellRead fields (e.g., Oklch with reactive lightness/chroma/hue).
    if let IrExpr::TaggedObject { tag, fields } = expr {
        if tag != "Oklch" {
            return el;
        }
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
            let store = instance.cell_store.clone();
            return el.style_signal(
                css_prop,
                store.get_cell_signal(cell_id).map(move |_| {
                    let l =
                        lightness_cell.map_or(lightness_default, |cid| store.get_cell_value(cid));
                    let c = chroma_cell.map_or(chroma_default, |cid| store.get_cell_value(cid));
                    let h = hue_cell.map_or(hue_default, |hid| store.get_cell_value(hid));
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
    el
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
