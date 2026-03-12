//! DOM bridge — creates Zoon UI elements from the IR program and connects
//! them to the WASM runtime instance.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use boon_renderer_zoon::{custom_call_placeholder, unknown_placeholder, with_render_root};
use boon_scene::PhysicalSceneParams;
use futures_channel::mpsc;
use futures_signals::signal_vec::{SignalVec, VecDiff};
use pin_project::pin_project;
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
use super::runtime::{CellStore, ItemCellStore, ListStore, WasmInstance};

const LIST_MAP_TOP_LEVEL_INCREMENTAL_THRESHOLD: usize = 1;
const LIST_MAP_TOP_LEVEL_CHUNK_SIZE: usize = 1;
const LIST_MAP_NESTED_INCREMENTAL_THRESHOLD: usize = 1;
const LIST_MAP_NESTED_CHUNK_SIZE: usize = 1;
const LIST_MAP_INCREMENTAL_YIELD_MS: u32 = 0;
const RUNTIME_RESOLVE_DEPTH_LIMIT: u32 = 128;

#[derive(Clone, Copy)]
struct ListMapRenderPlan {
    use_incremental: bool,
    chunk_size: usize,
}

fn list_map_render_plan(
    parent_item_ctx: Option<&ItemContext>,
    item_count: usize,
) -> ListMapRenderPlan {
    if parent_item_ctx.is_some() {
        ListMapRenderPlan {
            use_incremental: item_count > LIST_MAP_NESTED_INCREMENTAL_THRESHOLD,
            chunk_size: LIST_MAP_NESTED_CHUNK_SIZE,
        }
    } else {
        ListMapRenderPlan {
            use_incremental: item_count > LIST_MAP_TOP_LEVEL_INCREMENTAL_THRESHOLD,
            chunk_size: LIST_MAP_TOP_LEVEL_CHUNK_SIZE,
        }
    }
}

#[pin_project]
struct VecDiffStreamSignalVec<A>(#[pin] A);

impl<A, T> SignalVec for VecDiffStreamSignalVec<A>
where
    A: Stream<Item = VecDiff<T>>,
{
    type Item = T;

    fn poll_vec_change(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context,
    ) -> std::task::Poll<Option<VecDiff<Self::Item>>> {
        self.project().0.poll_next(cx)
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Build the Zoon element tree for the given IR program and WASM instance.
/// The tag_table is cloned for reactive text display.
pub fn build_ui(program: &IrProgram, instance: Rc<WasmInstance>) -> RawElOrText {
    with_render_root(program.render_root(), |render_root| {
        initialize_scene_params(&instance);
        build_cell_element(program, &instance, render_root.root)
    })
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
            | IrNode::StreamSkip { .. }
            | IrNode::ListCount { .. }
            | IrNode::HoldLoop { .. }
            | IrNode::ListIsEmpty { .. }
            | IrNode::ListEvery { .. }
            | IrNode::ListAny { .. }
            | IrNode::TextTrim { .. }
            | IrNode::TextIsNotEmpty { .. }
            | IrNode::TextToNumber { .. }
            | IrNode::MathRound { .. }
            | IrNode::MathMin { .. }
            | IrNode::MathMax { .. }
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
                ctx.item_ctx().cloned(),
                cell,
                *source,
                *item_cell,
                item_name,
                template,
                *template_cell_range,
                *template_event_range,
            ),
            IrNode::Document { root, .. } => build_element(program, instance, ctx, *root),
            IrNode::CustomCall { path, .. } => custom_call_placeholder(path),
            _ => unknown_placeholder(),
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
        _ => build_reactive_text(instance, cell),
    }
}

/// Build a reactive text element that shows the cell's display value.
fn build_reactive_text(instance: &Rc<WasmInstance>, cell: CellId) -> RawElOrText {
    let store = instance.cell_store.clone();
    let program = instance.program.clone();
    let cell_id = cell.0;
    let signal = store.get_revision_signal();
    zoon::Text::with_signal(
        signal.map(move |_| format_runtime_cell_value(&program, &store, CellId(cell_id))),
    )
    .unify()
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

fn visible_tag_text(tag: &str) -> Option<String> {
    (tag != "NaN").then(|| tag.to_string())
}

fn format_runtime_cell_value(
    program: &IrProgram,
    store: &super::runtime::CellStore,
    cell: CellId,
) -> String {
    match resolve_runtime_cell_expr(program, store, cell, 0).unwrap_or(IrExpr::CellRead(cell)) {
        IrExpr::Constant(IrValue::Text(t)) => t,
        IrExpr::Constant(IrValue::Number(n)) => format_number(n),
        IrExpr::Constant(IrValue::Tag(t)) => t,
        IrExpr::CellRead(cell) => format_cell_value(store, cell.0),
        _ => format_cell_value(store, cell.0),
    }
}

fn format_runtime_cell_value_with_bindings(
    program: &IrProgram,
    store: &super::runtime::CellStore,
    cell: CellId,
    bindings: &HashMap<CellId, IrExpr>,
) -> String {
    match resolve_runtime_cell_expr_with_bindings(program, store, cell, 0, bindings)
        .unwrap_or(IrExpr::CellRead(cell))
    {
        IrExpr::Constant(IrValue::Text(text)) => text,
        IrExpr::Constant(IrValue::Number(number)) => format_number(number),
        IrExpr::Constant(IrValue::Tag(tag)) => visible_tag_text(&tag).unwrap_or_default(),
        IrExpr::CellRead(next) => format_cell_value(store, next.0),
        _ => format_cell_value(store, cell.0),
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
        ElementKind::SvgCircle { cx, cy, r, style } => {
            build_svg_circle(program, instance, &BuildContext::Global, cx, cy, r, style)
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
        if matches!(
            find_node_for_cell(program, *cell),
            Some(IrNode::Element { .. })
        ) {
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
    fn resolve_field_map(
        program: &IrProgram,
        cell: CellId,
        depth: usize,
    ) -> Option<HashMap<String, CellId>> {
        if depth > 16 {
            return None;
        }
        if let Some(fields) = program.cell_field_cells.get(&cell) {
            return Some(fields.clone());
        }
        for node in &program.nodes {
            match node {
                IrNode::Derived {
                    cell: c,
                    expr: IrExpr::TaggedObject { fields, .. },
                }
                | IrNode::Derived {
                    cell: c,
                    expr: IrExpr::ObjectConstruct(fields),
                } if *c == cell => {
                    let mut field_map = HashMap::new();
                    for (field_name, field_expr) in fields {
                        if let IrExpr::CellRead(field_cell) = field_expr {
                            field_map.insert(field_name.clone(), *field_cell);
                        }
                    }
                    if !field_map.is_empty() {
                        return Some(field_map);
                    }
                }
                IrNode::Derived {
                    cell: c,
                    expr: IrExpr::CellRead(source),
                } if *c == cell => return resolve_field_map(program, *source, depth + 1),
                IrNode::PipeThrough { cell: c, source } if *c == cell => {
                    return resolve_field_map(program, *source, depth + 1);
                }
                IrNode::Derived {
                    cell: c,
                    expr: IrExpr::FieldAccess { object, field },
                } if *c == cell => {
                    let IrExpr::CellRead(object_cell) = object.as_ref() else {
                        return None;
                    };
                    let object_fields = resolve_field_map(program, *object_cell, depth + 1)?;
                    let field_cell = *object_fields.get(field)?;
                    return resolve_field_map(program, field_cell, depth + 1);
                }
                _ => {}
            }
        }
        None
    }

    fn resolve_scalar_leaf_cell(program: &IrProgram, start: CellId) -> CellId {
        let mut current = start;
        for _ in 0..16 {
            let Some(fields) = resolve_field_map(program, current, 0) else {
                break;
            };
            if fields.len() != 1 {
                break;
            }
            if let Some(next) = fields.values().next() {
                current = *next;
            } else {
                break;
            }
        }
        current
    }

    let press_event = links
        .iter()
        .find(|(name, _)| name == "press")
        .map(|(_, eid)| *eid);
    let label_child = build_label_child(program, instance, ctx, label);
    let inst_press = instance.clone();
    let item_ctx = ctx.item_ctx().cloned();
    let item_label_cell = match (ctx.item_ctx(), label) {
        (Some(item_ctx), IrExpr::CellRead(cell)) if item_ctx.is_template_cell(*cell) => {
            Some(cell.0)
        }
        _ => None,
    };
    let item_id_scalar_cell = ctx.item_ctx().and_then(|item_ctx| {
        program
            .cell_field_cells
            .get(&CellId(item_ctx.item_cell_id))
            .and_then(|fields| fields.get("id"))
            .map(|cell| resolve_scalar_leaf_cell(program, *cell).0)
    });
    let fire_press: Rc<dyn Fn()> = Rc::new(move || {
        if let Some(event_id) = press_event {
            if let Some(item_ctx) = item_ctx.as_ref() {
                let _ = inst_press.call_on_item_event(
                    item_ctx.item_idx,
                    event_id.0,
                    item_ctx.propagation_item_idx,
                );
            } else {
                let _ = inst_press.fire_event(event_id.0);
            }
        }
    });
    if ctx.item_ctx().is_some() {
        let mut el = El::new();
        el = apply_typed_styles(el, style, program, false);
        if let Some(cell) = hovered_cell {
            el = apply_hover(el, instance, ctx.item_ctx(), cell);
        }
        el.child(label_child)
            .update_raw_el(|raw_el| {
                let fire_press = fire_press.clone();
                let raw_el = raw_el
                    .attr("role", "button")
                    .attr("tabindex", "0")
                    .style("cursor", "pointer")
                    .style("user-select", "none")
                    .event_handler(move |_: events::MouseDown| fire_press());
                let raw_el = apply_raw_css(raw_el, style, program, instance, ctx.item_ctx(), false);
                apply_physical_css(raw_el, style, program, instance, ctx.item_ctx())
            })
            .into_raw_unchecked()
    } else {
        let mut btn = Button::new()
            .label(label_child)
            .on_press(move || fire_press());
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
                                if let Some((source, arms)) = cond {
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
                _ => Vec::new(),
            }
        }
        Some(IrNode::While { source, arms, .. }) | Some(IrNode::When { source, arms, .. }) => {
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
    let mut change_text_cells = find_data_cells_for_event(program, links, "change", "text");
    let mut text_cells = find_text_property_cells(program, links);
    if let Some(local_text_cell) = boon_text_cell {
        text_cells.push(local_text_cell.0);
        if let Some(base_name) = program.cells[local_text_cell.0 as usize]
            .name
            .strip_suffix(".text")
        {
            let local_change_text_name = format!("{}.event.change.text", base_name);
            if let Some(cell_id) =
                program.cells.iter().enumerate().find_map(|(idx, info)| {
                    (info.name == local_change_text_name).then_some(idx as u32)
                })
            {
                change_text_cells.push(cell_id);
            }
        }
    }
    change_text_cells.sort_unstable();
    change_text_cells.dedup();
    text_cells.sort_unstable();
    text_cells.dedup();
    let text_cell = text_cells
        .iter()
        .copied()
        .find(|cell_id| boon_text_cell.is_none() || Some(CellId(*cell_id)) == boon_text_cell);
    let inherit_text_color = !style_defines_font_color(style, program);

    let ti = TextInput::new();
    let ti = apply_typed_styles(ti, style, program, false);
    let ti = if let Some(cell) = hovered_cell {
        apply_hover(ti, instance, None, cell)
    } else {
        ti
    };
    macro_rules! finish_text_input {
        ($ti:expr) => {{
            let ph_el = build_placeholder(placeholder, program);
            $ti.placeholder(ph_el)
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

                    let mount_key_down_event = key_down_event.map(|event_id| event_id.0);
                    let mount_change_event = change_event.map(|event_id| event_id.0);
                    let mount_text_cell = text_cell;
                    let mount_boon_text_cell = boon_text_cell.map(|cell_id| cell_id.0);
                    let _ = (
                        mount_key_down_event,
                        mount_change_event,
                        mount_text_cell,
                        mount_boon_text_cell,
                    );

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
                            let handle = Task::start_droppable(
                                signal_store
                                    .get_cell_signal(cell_id)
                                    .for_each_sync(move |_| {
                                        let text = read_store.get_cell_text(cell_id);
                                        if input_for_signal.value() != text {
                                            input_for_signal.set_value(&text);
                                        }
                                    }),
                            );
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
                            let handle = Task::start_droppable(
                                store.get_cell_signal(btc_id).for_each_sync(move |_| {
                                    let text = read_store.get_cell_text(btc_id);
                                    if input_el.value() != text {
                                        input_el.set_value(&text);
                                    }
                                }),
                            );
                            std::mem::forget(handle);
                        });
                    }

                    raw_el = apply_raw_css(raw_el, style, program, instance, None, false);
                    raw_el = apply_physical_css(raw_el, style, program, instance, None);

                    if let Some(event_id) = change_event {
                        let inst = instance.clone();
                        let change_text_cells_for_input = change_text_cells.clone();
                        let text_cells_for_input = text_cells.clone();
                        raw_el = raw_el.event_handler(move |event: events::Input| {
                            if let Some(target) = event.target() {
                                if let Ok(input) = target.dyn_into::<web_sys::HtmlInputElement>() {
                                    let text = input.value();
                                    for &cell_id in &change_text_cells_for_input {
                                        inst.cell_store.set_cell_text(cell_id, text.clone());
                                    }
                                    for &cell_id in &text_cells_for_input {
                                        inst.cell_store.set_cell_text(cell_id, text.clone());
                                    }
                                }
                            }
                            let _ = inst.fire_event(event_id.0);
                        });
                    }

                    // Set up keydown event listener.
                    let raw_el = if let Some(event_id) = key_down_event {
                        let inst = instance.clone();
                        let change_event_for_key = change_event;
                        let change_text_cells_for_key = change_text_cells.clone();
                        let text_cells_for_key = text_cells.clone();
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
                                    for &cell_id in &change_text_cells_for_key {
                                        inst.cell_store.set_cell_text(cell_id, input_text.clone());
                                    }
                                    for &cell_id in &text_cells_for_key {
                                        inst.cell_store.set_cell_text(cell_id, input_text.clone());
                                    }
                                    if key == "Enter" {
                                        if let Some(change_eid) = change_event_for_key {
                                            let _ = inst.fire_event(change_eid.0);
                                        }
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

                    raw_el
                })
                .into_raw_unchecked()
        }};
    }

    finish_text_input!(ti)
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
    let change_value_cells = find_data_cells_for_event(program, links, "change", "value");

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
                    if let Some(cell) = value_cell {
                        inst.set_cell_value(cell.0, val);
                    }
                    for cell_id in &change_value_cells {
                        inst.set_cell_value(*cell_id, val);
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
    let change_value_cells = find_data_cells_for_event(program, links, "change", "value");

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

    // Handle change event from the browser select element.
    if let Some(event_id) = change_event {
        let inst = instance.clone();
        let apply_select_change = move |target: web_sys::EventTarget| {
            // HtmlSelectElement feature not enabled in web_sys, so get value via js_sys.
            let value_key = wasm_bindgen::JsValue::from_str("value");
            if let Ok(val) = js_sys::Reflect::get(&target, &value_key) {
                let selected_value = val.as_string().unwrap_or_default();
                for cell_id in &change_value_cells {
                    inst.cell_store
                        .set_cell_text(*cell_id, selected_value.clone());
                }
            }
            let _ = inst.fire_event(event_id.0);
        };
        let apply_select_input = apply_select_change.clone();
        raw_el = raw_el.event_handler(move |event: events::Input| {
            if let Some(target) = event.target() {
                apply_select_input(target);
            }
        });
        raw_el = raw_el.event_handler(move |event: events::Change| {
            if let Some(target) = event.target() {
                apply_select_change(target);
            }
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

fn find_data_cells_for_event(
    program: &IrProgram,
    links: &[(String, EventId)],
    event_name: &str,
    field_name: &str,
) -> Vec<u32> {
    let Some(event_id) = links
        .iter()
        .find(|(name, _)| name == event_name)
        .map(|(_, eid)| *eid)
    else {
        return Vec::new();
    };

    let event_info = &program.events[event_id.0 as usize];
    let field_suffix = format!(".event.{}.{}", event_name, field_name);
    let mut cells: Vec<u32> = event_info
        .payload_cells
        .iter()
        .filter_map(|cid| {
            program
                .cells
                .get(cid.0 as usize)
                .is_some_and(|info| info.name.ends_with(&field_suffix))
                .then_some(cid.0)
        })
        .collect();

    if cells.is_empty() {
        if let Some(cell) = find_data_cell_for_event(program, links, event_name, field_name) {
            cells.push(cell);
        }
    } else {
        cells.sort_unstable();
        cells.dedup();
    }

    cells
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
    let event_info = &program.events[event_id.0 as usize];
    let element_path = event_info.name.strip_suffix(&format!(".{}", event_name))?;
    let data_cell_name = format!("{}.event.{}.{}", element_path, event_name, field_name);

    let exact_match = |cid: &CellId| {
        program
            .cells
            .get(cid.0 as usize)
            .is_some_and(|info| info.name == data_cell_name)
    };
    if let Some((start, end)) = template_range {
        if let Some(cell_id) = event_info
            .payload_cells
            .iter()
            .copied()
            .find(|cid| cid.0 >= start && cid.0 < end && exact_match(cid))
        {
            return Some(cell_id.0);
        }
    }
    if let Some(cell_id) = event_info.payload_cells.iter().copied().find(exact_match) {
        return Some(cell_id.0);
    }

    let field_suffix = format!(".event.{}.{}", event_name, field_name);

    // Prefer payload cells explicitly declared on the event. This is more robust
    // than reconstructing the element path from the event name because LINK
    // lowering can reuse event ids across local and linked element aliases.
    let payload_match = |cid: &CellId| {
        program
            .cells
            .get(cid.0 as usize)
            .is_some_and(|info| info.name.ends_with(&field_suffix))
    };
    if let Some((start, end)) = template_range {
        if let Some(cell_id) = event_info
            .payload_cells
            .iter()
            .copied()
            .find(|cid| cid.0 >= start && cid.0 < end && payload_match(cid))
        {
            return Some(cell_id.0);
        }
    }
    if let Some(cell_id) = event_info.payload_cells.iter().copied().find(payload_match) {
        return Some(cell_id.0);
    }

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

fn find_text_property_cells(program: &IrProgram, links: &[(String, EventId)]) -> Vec<u32> {
    let Some(event_id) = links.first().map(|(_, eid)| *eid) else {
        return Vec::new();
    };
    let event_info = &program.events[event_id.0 as usize];
    let Some(dot_pos) = event_info.name.rfind('.') else {
        return Vec::new();
    };
    let element_path = &event_info.name[..dot_pos];
    let text_cell_name = format!("{}.text", element_path);

    let mut cells: Vec<u32> = program
        .cells
        .iter()
        .enumerate()
        .filter_map(|(idx, info)| (info.name == text_cell_name).then_some(idx as u32))
        .collect();

    if cells.is_empty() {
        if let Some(cell) = find_text_property_cell(program, links) {
            cells.push(cell);
        }
    } else {
        cells.sort_unstable();
        cells.dedup();
    }

    cells
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
                    IrExpr::CellRead(cell) => Some(build_element(program, instance, ctx, *cell)),
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

fn encoded_runtime_tag_value(tag_table: &[String], tag: &str) -> f64 {
    if let Some(idx) = tag_table.iter().position(|t| t == tag) {
        (idx + 1) as f64
    } else if tag == "True" {
        1.0
    } else if tag == "False" {
        0.0
    } else {
        f64::NAN
    }
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
    propagation_item_idx: u32,
    item_cell_id: u32,
    resolved_item_store_id: u32,
    template_cell_range: (u32, u32),
    template_event_range: (u32, u32),
    item_expr: Option<IrExpr>,
}

impl ItemContext {
    fn is_template_cell(&self, cell: CellId) -> bool {
        cell.0 >= self.template_cell_range.0 && cell.0 < self.template_cell_range.1
    }

    fn is_template_event(&self, event: EventId) -> bool {
        event.0 >= self.template_event_range.0 && event.0 < self.template_event_range.1
    }
}

fn bind_item_object_fields(
    program: &IrProgram,
    bindings: &mut HashMap<CellId, IrExpr>,
    object_cell: CellId,
    expr: &IrExpr,
) {
    let fields = match expr {
        IrExpr::ObjectConstruct(fields) | IrExpr::TaggedObject { fields, .. } => fields,
        _ => return,
    };

    let Some(field_cells) = program.cell_field_cells.get(&object_cell) else {
        return;
    };

    for (name, value) in fields {
        let Some(field_cell) = field_cells.get(name) else {
            continue;
        };
        bindings.insert(*field_cell, value.clone());
        bind_item_object_fields(program, bindings, *field_cell, value);
    }
}

fn item_bindings(
    program: &IrProgram,
    item_cell: CellId,
    resolved_item_store: CellId,
    item_expr: &IrExpr,
) -> HashMap<CellId, IrExpr> {
    let mut bindings = HashMap::new();
    bindings.insert(item_cell, item_expr.clone());
    bindings.insert(resolved_item_store, item_expr.clone());
    bind_item_object_fields(program, &mut bindings, item_cell, item_expr);
    if resolved_item_store != item_cell {
        bind_item_object_fields(program, &mut bindings, resolved_item_store, item_expr);
    }
    bindings
}

fn is_item_local_event(program: &IrProgram, ctx: &ItemContext, event: EventId) -> bool {
    if ctx.is_template_event(event) {
        return true;
    }
    let Some(event_info) = program.events.get(event.0 as usize) else {
        return false;
    };
    event_info.name.starts_with("object.") || event_info.name.starts_with("element.")
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
            BuildContext::Item(ctx)
                if is_item_local_event(instance.program.as_ref(), ctx, event) =>
            {
                let _ =
                    instance.call_on_item_event(ctx.item_idx, event.0, ctx.propagation_item_idx);
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
    parent_item_ctx: Option<ItemContext>,
    map_cell: CellId,
    source: CellId,
    item_cell: CellId,
    item_name: &str,
    template: &IrNode,
    template_cell_range: (u32, u32),
    template_event_range: (u32, u32),
) -> RawElOrText {
    let store = instance.cell_store.clone();
    let _ = item_name;
    let _ = template;
    let _ = template_cell_range;
    let _ = template_event_range;

    let list_map_plan = program
        .list_map_plan(map_cell)
        .cloned()
        .unwrap_or_else(|| panic!("missing ListMapPlan for map cell {}", map_cell.0));

    // Get the version signal from the map cell to trigger re-renders.
    let version_signal = store.get_cell_signal(map_cell.0);

    // Collect cross-scope event IDs (global events that trigger template-scoped nodes).
    // These need to be forwarded to all relevant items when the global event fires.
    let cross_scope_events = list_map_plan.template.cross_scope_events.clone();

    // For fanout, prefer the backing (unfiltered) source list. This keeps
    // cross-scope item updates (e.g. toggle-all) working even when the map's
    // visible source is filtered to zero items.
    let fanout_source = list_map_plan.fanout_source;
    let template_global_deps = list_map_plan.template.global_deps.clone();
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
        let hook_parent_item_ctx = parent_item_ctx.clone();
        inst.add_post_event_hook(Box::new(move |event_id, propagated_item_idx| {
            if cross_events.contains(&event_id) {
                let list_id = current_list_id_for_source(
                    &inst_hook,
                    hook_parent_item_ctx.as_ref(),
                    fanout_source,
                );
                let text_items = inst_hook.list_store.items_text(list_id);
                let f64_items = inst_hook.list_store.items(list_id);
                let item_count = if !text_items.is_empty() {
                    text_items.len()
                } else {
                    f64_items.len()
                };

                let target_item_indices: Vec<u32> =
                    if let Some(target_item_idx) = propagated_item_idx {
                        (0..item_count)
                            .map(|pos| inst_hook.list_store.item_memory_index(list_id, pos) as u32)
                            .filter(|item_idx| *item_idx == target_item_idx)
                            .collect()
                    } else {
                        (0..item_count)
                            .map(|pos| inst_hook.list_store.item_memory_index(list_id, pos) as u32)
                            .collect()
                    };

                for item_idx in target_item_indices {
                    if let Some(ref ics) = inst_hook.item_cell_store {
                        if !ics.has_item(item_idx) {
                            ics.ensure_item(item_idx);
                        }
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
                    let _ = inst_hook.call_on_item_event_batch(item_idx, event_id, item_idx);
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
    let template_global_revision = Mutable::new(0_u64);
    let dependency_watchers: Rc<Vec<TaskHandle>> = Rc::new(
        template_global_deps
            .iter()
            .map(|dep| {
                let dep_id = dep.0;
                let store = instance.cell_store.clone();
                let revision = template_global_revision.clone();
                Task::start_droppable(store.get_cell_signal(dep_id).for_each_sync(move |_| {
                    revision.set(revision.get().wrapping_add(1));
                }))
            })
            .collect(),
    );
    let trigger_signal: Box<dyn Signal<Item = (f64, u64)> + Unpin> =
        if template_global_deps.is_empty() {
            Box::new(version_signal.map(|version| (version, 0)))
        } else {
            Box::new(map_ref! {
                let version = version_signal,
                let revision = template_global_revision.signal() => (*version, *revision)
            })
        };
    let inst_dedup = instance.clone();
    let prev_indices: Rc<RefCell<Vec<u32>>> = Rc::new(RefCell::new(Vec::new()));
    let prev_global_snapshot = Rc::new(RefCell::new(None::<Vec<(u64, String)>>));
    let resolved_item_store = list_map_plan.resolved_item_store;
    let dedup_parent_item_ctx = parent_item_ctx.clone();
    let dedup_template_global_deps = template_global_deps.clone();
    let deduped_signal = trigger_signal.filter_map(move |(_version, _global_revision)| {
        let current_list_id =
            current_list_id_for_source(&inst_dedup, dedup_parent_item_ctx.as_ref(), source);
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
        let current_global_snapshot: Vec<(u64, String)> = dedup_template_global_deps
            .iter()
            .map(|cell| {
                (
                    inst_dedup.cell_store.get_cell_value(cell.0).to_bits(),
                    inst_dedup.cell_store.get_cell_text(cell.0),
                )
            })
            .collect();
        let mut prev = prev_indices.borrow_mut();
        let mut prev_snapshot = prev_global_snapshot.borrow_mut();
        let list_changed = *prev != current;
        let global_changed = prev_snapshot
            .as_ref()
            .is_none_or(|snapshot| snapshot != &current_global_snapshot);
        if !list_changed && !global_changed {
            None // Same items and same dependency values — suppress signal.
        } else {
            *prev = current;
            *prev_snapshot = Some(current_global_snapshot);
            Some(global_changed)
        }
    });
    let render_count = Rc::new(RefCell::new(0_u32));

    // Build a map: field_name → HOLD init source cell for each field in item_cell.
    // This allows us to seed the correct text on the HOLD's source cell BEFORE
    // init_item runs, so that host_copy_text propagates the right text through
    // the entire template (including derived text interpolations like labels).
    let field_hold_sources = list_map_plan.field_hold_sources.clone();

    // Build a container that reactively re-renders children when the list changes.
    RawHtmlEl::new("div")
        .style("display", "contents")
        .child_signal(deduped_signal.map({
            let child_parent_item_ctx = parent_item_ctx.clone();
            let render_count = render_count.clone();
            move |global_deps_changed| {
                let global_deps_changed = global_deps_changed.unwrap_or(false);
                let _keep_dependency_watchers_alive = dependency_watchers.len();
                // Re-read list_id from source cell each time — the filter loop may
                // have replaced the list with a new filtered copy.
                let current_list_id =
                    current_list_id_for_source(&inst, child_parent_item_ctx.as_ref(), source);
                let text_items = inst.list_store.items_text(current_list_id);
                let f64_items = inst.list_store.items(current_list_id);
                let item_count = if !text_items.is_empty() {
                    text_items.len()
                } else {
                    f64_items.len()
                };
                {
                    let mut count = render_count.borrow_mut();
                    *count += 1;
                }
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

                let program = inst.program.clone();
                let has_pending_snapshot = inst.has_pending_snapshot();
                let mut render_plan =
                    list_map_render_plan(child_parent_item_ctx.as_ref(), item_count);
                // Nested rows in examples like Cells are small, but they depend on
                // global selector state (`editing_cell`, `editing_text`, etc.).
                // Building them incrementally can freeze early children against a
                // stale selector snapshot while later children see settled globals.
                // Render those nested lists coherently in one pass instead.
                if child_parent_item_ctx.is_some() && !template_global_deps.is_empty() {
                    render_plan.use_incremental = false;
                }
                if global_deps_changed && !template_global_deps.is_empty() {
                    let _ = template_global_deps;
                }

                if render_plan.use_incremental {
                    let (tx, rx) = mpsc::unbounded();
                    let program = program.clone();
                    let inst_for_task = inst.clone();
                    let initialized_for_task = initialized_indices.clone();
                    let field_hold_sources = field_hold_sources.clone();
                    let text_items = text_items.clone();
                    let f64_items = f64_items.clone();
                    let template_global_deps = template_global_deps.clone();
                    let parent_item_ctx = child_parent_item_ctx.clone();
                    let current_list_id = current_list_id;
                    let source = source;
                    let map_cell = map_cell;
                    let item_cell = item_cell;
                    let template = list_map_plan.template.clone();
                    let resolved_item_store = resolved_item_store;
                    let has_pending_snapshot = has_pending_snapshot;
                    let global_deps_changed = global_deps_changed;
                    let item_count = item_count;
                    let should_finalize_render = child_parent_item_ctx.is_none();

                    Task::start(async move {
                        let _ = tx.unbounded_send(VecDiff::Replace { values: vec![] });
                        let mut initialized_any = false;

                        for chunk_start in (0..item_count).step_by(render_plan.chunk_size) {
                            let chunk_end = (chunk_start + render_plan.chunk_size).min(item_count);

                            for i in chunk_start..chunk_end {
                                let (child, did_init) = build_list_map_child(
                                    program.as_ref(),
                                    &inst_for_task,
                                    parent_item_ctx.as_ref(),
                                    current_list_id,
                                    source,
                                    map_cell,
                                    item_cell,
                                    &template,
                                    resolved_item_store,
                                    &field_hold_sources,
                                    &initialized_for_task,
                                    &text_items,
                                    &f64_items,
                                    i,
                                    has_pending_snapshot,
                                    &template_global_deps,
                                    global_deps_changed,
                                );
                                initialized_any |= did_init;
                                if tx.unbounded_send(VecDiff::Push { value: child }).is_err() {
                                    return;
                                }
                            }

                            if chunk_end < item_count {
                                Timer::sleep(LIST_MAP_INCREMENTAL_YIELD_MS).await;
                            }
                        }

                        if should_finalize_render {
                            if initialized_any {
                                let _ = inst_for_task.call_rerun_retain_filters();
                            }
                            inst_for_task.finalize_restore();
                            inst_for_task.enable_save();
                        }
                    });

                    Some(
                        RawHtmlEl::new("div")
                            .style("display", "contents")
                            .children_signal_vec(VecDiffStreamSignalVec(rx))
                            .into_raw_unchecked(),
                    )
                } else {
                    let mut initialized_any = false;
                    let children: Vec<RawElOrText> = (0..item_count)
                        .map(|i| {
                            let (child, did_init) = build_list_map_child(
                                program.as_ref(),
                                &inst,
                                child_parent_item_ctx.as_ref(),
                                current_list_id,
                                source,
                                map_cell,
                                item_cell,
                                &list_map_plan.template,
                                resolved_item_store,
                                &field_hold_sources,
                                &initialized_indices,
                                &text_items,
                                &f64_items,
                                i,
                                has_pending_snapshot,
                                &template_global_deps,
                                global_deps_changed,
                            );
                            initialized_any |= did_init;
                            child
                        })
                        .collect();

                    if child_parent_item_ctx.is_none() {
                        if initialized_any {
                            let _ = inst.call_rerun_retain_filters();
                        }

                        inst.finalize_restore();
                        inst.enable_save();
                    }

                    Some(
                        RawHtmlEl::new("div")
                            .style("display", "contents")
                            .children(children)
                            .into_raw_unchecked(),
                    )
                }
            }
        }))
        .into_raw_unchecked()
}

#[allow(clippy::too_many_arguments)]
fn build_list_map_child(
    program: &IrProgram,
    inst: &Rc<WasmInstance>,
    parent_item_ctx: Option<&ItemContext>,
    current_list_id: f64,
    source: CellId,
    map_cell: CellId,
    item_cell: CellId,
    template: &TemplatePlan,
    resolved_item_store: CellId,
    field_hold_sources: &HashMap<String, CellId>,
    initialized_indices: &Rc<RefCell<HashSet<u32>>>,
    text_items: &[String],
    f64_items: &[f64],
    i: usize,
    has_pending_snapshot: bool,
    template_global_deps: &[CellId],
    global_deps_changed: bool,
) -> (RawElOrText, bool) {
    let item_idx = inst.list_store.item_memory_index(current_list_id, i) as u32;

    if let Some(ref ics) = inst.item_cell_store {
        if !ics.has_item(item_idx) {
            ics.ensure_item(item_idx);
        }
    }

    if let Some(ref ics) = inst.item_cell_store {
        if !has_pending_snapshot || ics.get_text(item_idx, item_cell.0).is_empty() {
            if i < text_items.len() {
                ics.set_text(item_idx, item_cell.0, text_items[i].clone());
            } else if i < f64_items.len()
                && !program.cell_field_cells.contains_key(&resolved_item_store)
            {
                ics.set_text(item_idx, item_cell.0, format_number(f64_items[i]));
            }
        }
    }

    let item_expr_for_template =
        resolve_runtime_list_item_expr_for_context(program, inst, parent_item_ctx, map_cell, i);

    let already_initialized = initialized_indices.borrow().contains(&item_idx);

    if !already_initialized {
        if let Some(ref ics) = inst.item_cell_store {
            if let Some(ref item_expr) = item_expr_for_template {
                seed_item_template_cells_from_expr(
                    program,
                    inst,
                    ics,
                    item_idx,
                    Some(template),
                    item_cell,
                    resolved_item_store,
                    item_expr,
                );
            }
            let field_texts = inst.list_store.field_texts_for_mem_idx(item_idx);
            if !field_texts.is_empty() {
                for (name, source_cell) in field_hold_sources {
                    if let Some(text) = field_texts.get(name) {
                        if !text.is_empty() {
                            ics.set_text(item_idx, source_cell.0, text.clone());
                        }
                    }
                }
            }
        }
    }
    let mut did_init = false;
    if !already_initialized {
        let _ = inst.call_init_item(item_idx, map_cell.0);
        did_init = true;
        if let Some(ref ics) = inst.item_cell_store {
            if let Some(ref item_expr) = item_expr_for_template {
                seed_item_template_cells_from_expr(
                    program,
                    inst,
                    ics,
                    item_idx,
                    Some(template),
                    item_cell,
                    resolved_item_store,
                    item_expr,
                );
            }
        }
        initialized_indices.borrow_mut().insert(item_idx);
    } else if !template_global_deps.is_empty() && global_deps_changed {
        let _ = inst.call_refresh_item(item_idx, map_cell.0);
        if let Some(ref ics) = inst.item_cell_store {
            if let Some(ref item_expr) = item_expr_for_template {
                seed_item_template_cells_from_expr(
                    program,
                    inst,
                    ics,
                    item_idx,
                    Some(template),
                    item_cell,
                    resolved_item_store,
                    item_expr,
                );
            }
        }
    }

    let ctx = ItemContext {
        item_idx,
        propagation_item_idx: parent_item_ctx
            .map(|ctx| ctx.propagation_item_idx)
            .unwrap_or(item_idx),
        item_cell_id: item_cell.0,
        resolved_item_store_id: resolved_item_store.0,
        template_cell_range: template.cell_range,
        template_event_range: template.event_range,
        item_expr: item_expr_for_template.clone(),
    };

    let child = if let Some(root_cell) = template.root_cell {
        build_element(program, inst, &BuildContext::Item(&ctx), root_cell)
    } else if i < text_items.len() {
        zoon::Text::new(text_items[i].clone()).unify()
    } else if i < f64_items.len() {
        zoon::Text::new(format_number(f64_items[i])).unify()
    } else {
        unknown_placeholder()
    };

    (child, did_init)
}

fn current_list_id_for_source(
    instance: &Rc<WasmInstance>,
    parent_item_ctx: Option<&ItemContext>,
    source: CellId,
) -> f64 {
    if let Some(parent_ctx) = parent_item_ctx {
        if parent_ctx.is_template_cell(source) {
            if let Some(list_id) = resolve_parent_template_list_id(
                &instance.program,
                &instance.cell_store,
                &instance.list_store,
                &instance.template_list_items,
                instance.item_cell_store.as_ref(),
                parent_ctx,
                source,
                None,
            ) {
                return list_id;
            }
        }
    }
    instance.cell_store.get_cell_value(source.0)
}

fn resolve_parent_template_list_id(
    program: &IrProgram,
    cell_store: &CellStore,
    list_store: &ListStore,
    template_list_items: &Rc<RefCell<HashMap<u64, Vec<IrExpr>>>>,
    item_cell_store: Option<&ItemCellStore>,
    parent_ctx: &ItemContext,
    list_cell: CellId,
    source_cell: Option<CellId>,
) -> Option<f64> {
    let cache_list_id = |ics: &ItemCellStore, cell: CellId, list_id: f64| {
        if parent_ctx.is_template_cell(cell) {
            ics.set_text(parent_ctx.item_idx, cell.0, String::new());
            ics.set_cell(parent_ctx.item_idx, cell.0, list_id);
        }
    };

    if let Some(parent_item_expr) = parent_ctx.item_expr.as_ref() {
        let bindings = item_bindings(
            program,
            CellId(parent_ctx.item_cell_id),
            CellId(parent_ctx.resolved_item_store_id),
            parent_item_expr,
        );

        for target_cell in source_cell.into_iter().chain(std::iter::once(list_cell)) {
            let Some(resolved) = resolve_runtime_cell_expr_with_bindings(
                program,
                cell_store,
                target_cell,
                0,
                &bindings,
            ) else {
                continue;
            };
            let Some(list_id) = materialize_template_list_expr(
                program,
                list_store,
                template_list_items,
                cell_store,
                &resolved,
                0,
                &bindings,
            ) else {
                continue;
            };
            if let Some(ics) = item_cell_store {
                cache_list_id(ics, target_cell, list_id);
                cache_list_id(ics, list_cell, list_id);
            }
            return Some(list_id);
        }
    }

    if let Some(ics) = item_cell_store {
        let list_id = ics.get_value(parent_ctx.item_idx, list_cell.0);
        if !list_id.is_nan() && list_id > 0.0 {
            return Some(list_id);
        }
    }

    if let Some(source_cell) = source_cell {
        if let Some(ics) = item_cell_store {
            if parent_ctx.is_template_cell(source_cell) {
                let list_id = ics.get_value(parent_ctx.item_idx, source_cell.0);
                if !list_id.is_nan() && list_id > 0.0 {
                    cache_list_id(ics, list_cell, list_id);
                    return Some(list_id);
                }
            }
        }

        let list_id = cell_store.get_cell_value(source_cell.0);
        if !list_id.is_nan() && list_id > 0.0 {
            if let Some(ics) = item_cell_store {
                cache_list_id(ics, list_cell, list_id);
            }
            return Some(list_id);
        }
    }

    None
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
    let mut seen_ranges = HashSet::new();
    collect_cross_scope_events_in_range(
        program,
        template_cell_range,
        template_event_range,
        false,
        &mut seen_ranges,
        &mut result,
    );
    result
}

fn collect_cross_scope_events_in_range(
    program: &IrProgram,
    scan_cell_range: (u32, u32),
    root_event_range: (u32, u32),
    include_local_events: bool,
    seen_ranges: &mut HashSet<(u32, u32)>,
    result: &mut Vec<u32>,
) {
    if !seen_ranges.insert(scan_cell_range) {
        return;
    }
    let (cell_start, cell_end) = scan_cell_range;
    let (event_start, event_end) = root_event_range;

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
            IrNode::ListMap {
                template_cell_range: child_range,
                ..
            } if child_range.0 >= cell_start
                && child_range.1 <= cell_end
                && *child_range != scan_cell_range =>
            {
                collect_cross_scope_events_in_range(
                    program,
                    *child_range,
                    root_event_range,
                    true,
                    seen_ranges,
                    result,
                );
                continue;
            }
            _ => continue,
        };

        for t in triggers {
            if include_local_events || t < event_start || t >= event_end {
                if !result.contains(&t) {
                    result.push(t);
                }
            }
        }
    }
}

fn collect_template_global_dependencies(
    program: &IrProgram,
    template_cell_range: (u32, u32),
) -> Vec<CellId> {
    let mut deps = HashSet::new();
    let mut seen = HashSet::new();
    let mut seen_funcs = HashSet::new();
    let local_params = HashSet::new();
    let canonical_named_cells = canonical_named_cells(program);
    let nested_ranges: Vec<(u32, u32)> = program
        .nodes
        .iter()
        .filter_map(|node| match node {
            IrNode::ListMap {
                template_cell_range: nested,
                ..
            } if *nested != template_cell_range
                && nested.0 >= template_cell_range.0
                && nested.1 <= template_cell_range.1 =>
            {
                Some(*nested)
            }
            _ => None,
        })
        .collect();

    for cell_id in template_cell_range.0..template_cell_range.1 {
        if nested_ranges
            .iter()
            .any(|range| cell_id >= range.0 && cell_id < range.1)
        {
            continue;
        }
        collect_template_cell_dependencies(
            program,
            CellId(cell_id),
            template_cell_range,
            &local_params,
            &canonical_named_cells,
            &mut deps,
            &mut seen,
            &mut seen_funcs,
        );
    }

    let mut deps: Vec<_> = deps.into_iter().collect();
    deps.sort_by_key(|cell| cell.0);
    deps
}

fn canonical_named_cells(program: &IrProgram) -> HashMap<String, CellId> {
    let template_ranges: Vec<(u32, u32)> = program
        .nodes
        .iter()
        .filter_map(|node| match node {
            IrNode::ListMap {
                template_cell_range,
                ..
            } => Some(*template_cell_range),
            _ => None,
        })
        .collect();
    let in_template_range = |cell: CellId| {
        template_ranges
            .iter()
            .any(|(start, end)| cell.0 >= *start && cell.0 < *end)
    };

    let mut result = HashMap::new();
    for (idx, info) in program.cells.iter().enumerate() {
        let cell = CellId(idx as u32);
        match result.entry(info.name.clone()) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(cell);
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let existing = *entry.get();
                if in_template_range(existing) && !in_template_range(cell) {
                    entry.insert(cell);
                }
            }
        }
    }
    result
}

fn canonicalize_external_template_cell(
    program: &IrProgram,
    canonical_named_cells: &HashMap<String, CellId>,
    cell: CellId,
) -> CellId {
    let Some(info) = program.cells.get(cell.0 as usize) else {
        return cell;
    };
    canonical_named_cells
        .get(&info.name)
        .copied()
        .unwrap_or(cell)
}

fn effective_runtime_field_cell(
    program: &IrProgram,
    canonical_named_cells: &HashMap<String, CellId>,
    field_cell: CellId,
    bindings: &HashMap<CellId, IrExpr>,
) -> CellId {
    if bindings.contains_key(&field_cell)
        || find_node_for_cell(program, field_cell).is_some()
        || program.cell_field_cells.contains_key(&field_cell)
    {
        field_cell
    } else {
        canonicalize_external_template_cell(program, canonical_named_cells, field_cell)
    }
}

fn resolve_template_dependency_field_cell(
    program: &IrProgram,
    object: CellId,
    field: &str,
    depth: usize,
) -> Option<CellId> {
    if depth > 16 {
        return None;
    }
    let object_store = resolve_to_object_store(program, object);
    if let Some(fields) = program.cell_field_cells.get(&object_store) {
        if let Some(field_cell) = fields.get(field) {
            return Some(*field_cell);
        }
    }

    match find_node_for_cell(program, object) {
        Some(IrNode::Derived {
            expr:
                IrExpr::FieldAccess {
                    object: nested_object,
                    field: nested_field,
                },
            ..
        }) => {
            let IrExpr::CellRead(nested_cell) = nested_object.as_ref() else {
                return None;
            };
            let nested_field_cell = resolve_template_dependency_field_cell(
                program,
                *nested_cell,
                nested_field,
                depth + 1,
            )?;
            resolve_template_dependency_field_cell(program, nested_field_cell, field, depth + 1)
        }
        Some(IrNode::Derived {
            expr: IrExpr::CellRead(source),
            ..
        }) => resolve_template_dependency_field_cell(program, *source, field, depth + 1),
        Some(IrNode::PipeThrough { source, .. }) => {
            resolve_template_dependency_field_cell(program, *source, field, depth + 1)
        }
        _ => None,
    }
}

fn collect_template_cell_dependencies(
    program: &IrProgram,
    cell: CellId,
    template_cell_range: (u32, u32),
    local_param_cells: &HashSet<CellId>,
    canonical_named_cells: &HashMap<String, CellId>,
    deps: &mut HashSet<CellId>,
    seen: &mut HashSet<CellId>,
    seen_funcs: &mut HashSet<FuncId>,
) {
    if local_param_cells.contains(&cell) {
        return;
    }

    if !seen.insert(cell) {
        return;
    }

    if cell.0 < template_cell_range.0 || cell.0 >= template_cell_range.1 {
        deps.insert(canonicalize_external_template_cell(
            program,
            canonical_named_cells,
            cell,
        ));
        return;
    }

    let Some(node) = find_node_for_cell(program, cell) else {
        return;
    };

    match node {
        IrNode::Derived { expr, .. } => {
            collect_template_expr_dependencies(
                program,
                expr,
                template_cell_range,
                local_param_cells,
                canonical_named_cells,
                deps,
                seen,
                seen_funcs,
            );
        }
        IrNode::PipeThrough { source, .. }
        | IrNode::TextTrim { source, .. }
        | IrNode::TextIsNotEmpty { source, .. }
        | IrNode::TextToNumber { source, .. }
        | IrNode::MathRound { source, .. } => {
            collect_template_cell_dependencies(
                program,
                *source,
                template_cell_range,
                local_param_cells,
                canonical_named_cells,
                deps,
                seen,
                seen_funcs,
            );
        }
        IrNode::TextStartsWith { source, prefix, .. }
        | IrNode::MathMin {
            source, b: prefix, ..
        }
        | IrNode::MathMax {
            source, b: prefix, ..
        } => {
            collect_template_cell_dependencies(
                program,
                *source,
                template_cell_range,
                local_param_cells,
                canonical_named_cells,
                deps,
                seen,
                seen_funcs,
            );
            collect_template_cell_dependencies(
                program,
                *prefix,
                template_cell_range,
                local_param_cells,
                canonical_named_cells,
                deps,
                seen,
                seen_funcs,
            );
        }
        IrNode::When { source, arms, .. } => {
            collect_template_cell_dependencies(
                program,
                *source,
                template_cell_range,
                local_param_cells,
                canonical_named_cells,
                deps,
                seen,
                seen_funcs,
            );
            for (_, body) in arms {
                collect_template_expr_dependencies(
                    program,
                    body,
                    template_cell_range,
                    local_param_cells,
                    canonical_named_cells,
                    deps,
                    seen,
                    seen_funcs,
                );
            }
        }
        IrNode::While {
            source,
            deps: node_deps,
            arms,
            ..
        } => {
            collect_template_cell_dependencies(
                program,
                *source,
                template_cell_range,
                local_param_cells,
                canonical_named_cells,
                deps,
                seen,
                seen_funcs,
            );
            for dep in node_deps {
                collect_template_cell_dependencies(
                    program,
                    *dep,
                    template_cell_range,
                    local_param_cells,
                    canonical_named_cells,
                    deps,
                    seen,
                    seen_funcs,
                );
            }
            for (_, body) in arms {
                collect_template_expr_dependencies(
                    program,
                    body,
                    template_cell_range,
                    local_param_cells,
                    canonical_named_cells,
                    deps,
                    seen,
                    seen_funcs,
                );
            }
        }
        IrNode::Hold {
            init,
            trigger_bodies,
            ..
        } => {
            collect_template_expr_dependencies(
                program,
                init,
                template_cell_range,
                local_param_cells,
                canonical_named_cells,
                deps,
                seen,
                seen_funcs,
            );
            for (_, body) in trigger_bodies {
                collect_template_expr_dependencies(
                    program,
                    body,
                    template_cell_range,
                    local_param_cells,
                    canonical_named_cells,
                    deps,
                    seen,
                    seen_funcs,
                );
            }
        }
        IrNode::Latest { arms, .. } => {
            for arm in arms {
                collect_template_expr_dependencies(
                    program,
                    &arm.body,
                    template_cell_range,
                    local_param_cells,
                    canonical_named_cells,
                    deps,
                    seen,
                    seen_funcs,
                );
            }
        }
        _ => {}
    }
}

fn collect_template_expr_dependencies(
    program: &IrProgram,
    expr: &IrExpr,
    template_cell_range: (u32, u32),
    local_param_cells: &HashSet<CellId>,
    canonical_named_cells: &HashMap<String, CellId>,
    deps: &mut HashSet<CellId>,
    seen: &mut HashSet<CellId>,
    seen_funcs: &mut HashSet<FuncId>,
) {
    match expr {
        IrExpr::CellRead(cell) => {
            collect_template_cell_dependencies(
                program,
                *cell,
                template_cell_range,
                local_param_cells,
                canonical_named_cells,
                deps,
                seen,
                seen_funcs,
            );
        }
        IrExpr::FieldAccess { object, field } => {
            if let IrExpr::CellRead(object_cell) = object.as_ref() {
                if let Some(field_cell) =
                    resolve_template_dependency_field_cell(program, *object_cell, field, 0)
                {
                    collect_template_cell_dependencies(
                        program,
                        field_cell,
                        template_cell_range,
                        local_param_cells,
                        canonical_named_cells,
                        deps,
                        seen,
                        seen_funcs,
                    );
                    return;
                }
            }
            collect_template_expr_dependencies(
                program,
                object,
                template_cell_range,
                local_param_cells,
                canonical_named_cells,
                deps,
                seen,
                seen_funcs,
            );
        }
        IrExpr::TextConcat(parts) => {
            for part in parts {
                if let TextSegment::Expr(expr) = part {
                    collect_template_expr_dependencies(
                        program,
                        expr,
                        template_cell_range,
                        local_param_cells,
                        canonical_named_cells,
                        deps,
                        seen,
                        seen_funcs,
                    );
                }
            }
        }
        IrExpr::ObjectConstruct(fields) | IrExpr::TaggedObject { fields, .. } => {
            for (_, value) in fields {
                collect_template_expr_dependencies(
                    program,
                    value,
                    template_cell_range,
                    local_param_cells,
                    canonical_named_cells,
                    deps,
                    seen,
                    seen_funcs,
                );
            }
        }
        IrExpr::Compare { lhs, rhs, .. } | IrExpr::BinOp { lhs, rhs, .. } => {
            collect_template_expr_dependencies(
                program,
                lhs,
                template_cell_range,
                local_param_cells,
                canonical_named_cells,
                deps,
                seen,
                seen_funcs,
            );
            collect_template_expr_dependencies(
                program,
                rhs,
                template_cell_range,
                local_param_cells,
                canonical_named_cells,
                deps,
                seen,
                seen_funcs,
            );
        }
        IrExpr::UnaryNeg(inner) | IrExpr::Not(inner) => {
            collect_template_expr_dependencies(
                program,
                inner,
                template_cell_range,
                local_param_cells,
                canonical_named_cells,
                deps,
                seen,
                seen_funcs,
            );
        }
        IrExpr::FunctionCall { func, args } => {
            for arg in args {
                collect_template_expr_dependencies(
                    program,
                    arg,
                    template_cell_range,
                    local_param_cells,
                    canonical_named_cells,
                    deps,
                    seen,
                    seen_funcs,
                );
            }
            if !seen_funcs.insert(*func) {
                return;
            }
            if let Some(ir_func) = program.functions.get(func.0 as usize) {
                let nested_local_params: HashSet<CellId> = ir_func
                    .param_cells
                    .iter()
                    .copied()
                    .chain(local_param_cells.iter().copied())
                    .collect();
                collect_template_expr_dependencies(
                    program,
                    &ir_func.body,
                    template_cell_range,
                    &nested_local_params,
                    canonical_named_cells,
                    deps,
                    seen,
                    seen_funcs,
                );
            }
            seen_funcs.remove(func);
        }
        IrExpr::ListConstruct(items) => {
            for item in items {
                collect_template_expr_dependencies(
                    program,
                    item,
                    template_cell_range,
                    local_param_cells,
                    canonical_named_cells,
                    deps,
                    seen,
                    seen_funcs,
                );
            }
        }
        IrExpr::PatternMatch { source, arms } => {
            collect_template_cell_dependencies(
                program,
                *source,
                template_cell_range,
                local_param_cells,
                canonical_named_cells,
                deps,
                seen,
                seen_funcs,
            );
            for (_, body) in arms {
                collect_template_expr_dependencies(
                    program,
                    body,
                    template_cell_range,
                    local_param_cells,
                    canonical_named_cells,
                    deps,
                    seen,
                    seen_funcs,
                );
            }
        }
        IrExpr::Constant(_) => {}
    }
}

/// Build reactive text that reads from per-item cell store.
fn build_item_reactive_text(
    instance: &Rc<WasmInstance>,
    ctx: &ItemContext,
    cell: CellId,
) -> RawElOrText {
    fn resolve_item_text_fallback(
        program: &IrProgram,
        store: &CellStore,
        item_cell_id: u32,
        resolved_item_store_id: u32,
        item_expr: Option<&IrExpr>,
        cell: CellId,
    ) -> Option<String> {
        let item_expr = item_expr?;
        let bindings = item_bindings(
            program,
            CellId(item_cell_id),
            CellId(resolved_item_store_id),
            item_expr,
        );
        match resolve_runtime_cell_expr_with_bindings(program, store, cell, 0, &bindings)? {
            IrExpr::Constant(IrValue::Text(text)) => Some(text),
            IrExpr::Constant(IrValue::Tag(tag)) => visible_tag_text(&tag),
            IrExpr::Constant(IrValue::Number(number)) => Some(format_number(number)),
            _ => None,
        }
    }

    let ics = match &instance.item_cell_store {
        Some(ics) => ics.clone(),
        None => return build_reactive_text(instance, cell),
    };
    let item_idx = ctx.item_idx;
    let cell_id = cell.0;
    let item_cell_id = ctx.item_cell_id;
    let resolved_item_store_id = ctx.resolved_item_store_id;
    let item_expr = ctx.item_expr.clone();
    let store = instance.cell_store.clone();
    let program = instance.program.clone();
    let signal = ics.get_signal(item_idx, cell_id);
    zoon::Text::with_signal(signal.map(move |_sig_val| {
        let text = ics.get_text(item_idx, cell_id);
        let val = ics.get_value(item_idx, cell_id);
        if !text.is_empty() {
            text
        } else {
            if !val.is_nan() && val != 0.0 {
                format_number(val)
            } else {
                resolve_item_text_fallback(
                    &program,
                    &store,
                    item_cell_id,
                    resolved_item_store_id,
                    item_expr.as_ref(),
                    CellId(cell_id),
                )
                .unwrap_or_else(|| format_number(val))
            }
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
    fn resolve_item_segment_fallback(
        program: &IrProgram,
        store: &CellStore,
        item_cell_id: u32,
        resolved_item_store_id: u32,
        item_expr: Option<&IrExpr>,
        segment_cell: u32,
    ) -> Option<String> {
        let item_expr = item_expr?;
        let bindings = item_bindings(
            program,
            CellId(item_cell_id),
            CellId(resolved_item_store_id),
            item_expr,
        );
        match resolve_runtime_cell_expr_with_bindings(
            program,
            store,
            CellId(segment_cell),
            0,
            &bindings,
        )? {
            IrExpr::Constant(IrValue::Text(text)) => Some(text),
            IrExpr::Constant(IrValue::Tag(tag)) => visible_tag_text(&tag),
            IrExpr::Constant(IrValue::Number(number)) => Some(format_number(number)),
            _ => None,
        }
    }

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
    let item_cell_id = ctx.item_cell_id;
    let resolved_item_store_id = ctx.resolved_item_store_id;
    let item_expr_for_fallback = ctx.item_expr.clone();
    let program = instance.program.clone();

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
            let rendered = seg_desc
                .iter()
                .map(|s| match s {
                    ItemSegDesc::Lit(t) => t.clone(),
                    ItemSegDesc::ItemCell(id) => {
                        let text = ics.get_text(item_idx, *id);
                        if !text.is_empty() {
                            text
                        } else {
                            let value = ics.get_value(item_idx, *id);
                            if !value.is_nan() && value != 0.0 {
                                format_number(value)
                            } else {
                                resolve_item_segment_fallback(
                                    &program,
                                    &store,
                                    item_cell_id,
                                    resolved_item_store_id,
                                    item_expr_for_fallback.as_ref(),
                                    *id,
                                )
                                .unwrap_or_else(|| format_number(value))
                            }
                        }
                    }
                    ItemSegDesc::GlobalCell(id) => format_cell_value(&store, *id),
                })
                .collect::<String>();
            rendered
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
                            let value = ics.get_value(item_idx, *id);
                            if !value.is_nan() && value != 0.0 {
                                format_number(value)
                            } else {
                                resolve_item_segment_fallback(
                                    &program,
                                    &store,
                                    item_cell_id,
                                    resolved_item_store_id,
                                    item_expr_for_fallback.as_ref(),
                                    *id,
                                )
                                .unwrap_or_else(|| format_number(value))
                            }
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
        attach_item_events(program, raw, instance, ctx, links, remaining_hovered_cell)
    } else {
        let filtered: Vec<_> = links
            .iter()
            .filter(|(name, _)| !handled_events.contains(&name.as_str()))
            .cloned()
            .collect();
        attach_item_events(
            program,
            raw,
            instance,
            ctx,
            &filtered,
            remaining_hovered_cell,
        )
    }
}

/// Attach event handlers to a per-item element.
/// Template-scoped events route through on_item_event; global events through on_event.
/// Events are attached directly to the element (no wrapper div) to avoid
/// Chrome's buggy mouse events on `display: contents` elements.
fn attach_item_events(
    program: &IrProgram,
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
    let propagation_item_idx = ctx.propagation_item_idx;

    if let Some(event_id) = blur_event {
        let inst = instance.clone();
        let is_template = is_item_local_event(program, ctx, event_id);
        html_el = html_el.event_handler(move |_: events::Blur| {
            if is_template {
                let _ = inst.call_on_item_event(item_idx, event_id.0, propagation_item_idx);
            } else {
                let _ = inst.fire_event(event_id.0);
            }
        });
    }

    if let Some(event_id) = focus_event {
        let inst = instance.clone();
        let is_template = is_item_local_event(program, ctx, event_id);
        html_el = html_el.event_handler(move |_: events::Focus| {
            if is_template {
                let _ = inst.call_on_item_event(item_idx, event_id.0, propagation_item_idx);
            } else {
                let _ = inst.fire_event(event_id.0);
            }
        });
    }

    if let Some(event_id) = click_event {
        let inst = instance.clone();
        let is_template = is_item_local_event(program, ctx, event_id);
        html_el = html_el.event_handler(move |_: events::Click| {
            if is_template {
                let _ = inst.call_on_item_event(item_idx, event_id.0, propagation_item_idx);
            } else {
                let _ = inst.fire_event(event_id.0);
            }
        });
    }

    if let Some(event_id) = double_click_event {
        let inst = instance.clone();
        let is_template = is_item_local_event(program, ctx, event_id);
        html_el = html_el.event_handler(move |_: events::DoubleClick| {
            if is_template {
                let _ = inst.call_on_item_event(item_idx, event_id.0, propagation_item_idx);
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
    let propagation_item_idx = ctx.propagation_item_idx;
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

    let ti = TextInput::new();
    let ti = apply_typed_styles(ti, style, program, false);
    let ti = if let Some(cell) = hovered_cell {
        apply_hover(ti, instance, Some(ctx), cell)
    } else {
        ti
    };
    macro_rules! finish_item_text_input {
        ($ti:expr) => {{
            let ph_el = build_placeholder(placeholder, program);
            $ti.placeholder(ph_el)
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

                    if let Some(event_id) = change_event {
                        let inst = instance.clone();
                        let is_template = is_item_local_event(program, ctx, event_id);
                        let ics = ics_clone.clone();
                        let template_cell_range = ctx.template_cell_range;
                        raw_el = raw_el.event_handler(move |event: events::Input| {
                            if let Some(target) = event.target() {
                                if let Ok(input) = target.dyn_into::<web_sys::HtmlInputElement>() {
                                    let text = input.value();
                                    for cell_id_opt in [change_text_cell, text_cell] {
                                        if let Some(cell_id) = cell_id_opt {
                                            if cell_id >= template_cell_range.0
                                                && cell_id < template_cell_range.1
                                            {
                                                if let Some(ref ics) = ics {
                                                    ics.set_text(item_idx, cell_id, text.clone());
                                                }
                                            } else {
                                                inst.cell_store
                                                    .set_cell_text(cell_id, text.clone());
                                            }
                                        }
                                    }
                                }
                            }
                            if is_template {
                                let _ = inst.call_on_item_event(
                                    item_idx,
                                    event_id.0,
                                    propagation_item_idx,
                                );
                            } else {
                                let _ = inst.fire_event(event_id.0);
                            }
                        });
                    }

                    // Set up keydown event listener.
                    let raw_el = if let Some(event_id) = key_down_event {
                        let inst = instance.clone();
                        let is_template = is_item_local_event(program, ctx, event_id);
                        let change_event_for_key = change_event;
                        let change_is_template = change_event
                            .map(|eid| is_item_local_event(program, ctx, eid))
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
                                                    ics.set_text(
                                                        item_idx,
                                                        cell_id,
                                                        input_text.clone(),
                                                    );
                                                }
                                            } else {
                                                inst.cell_store
                                                    .set_cell_text(cell_id, input_text.clone());
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
                                        let _ = inst.call_on_item_event(
                                            item_idx,
                                            change_eid.0,
                                            propagation_item_idx,
                                        );
                                    } else {
                                        let _ = inst.fire_event(change_eid.0);
                                    }
                                }
                            }
                            if let Some(cell_id) = key_data_cell {
                                if cell_id >= template_cell_range.0
                                    && cell_id < template_cell_range.1
                                {
                                    inst.set_item_cell_value(item_idx, cell_id, tag_value);
                                } else {
                                    inst.set_cell_value(cell_id, tag_value);
                                }
                            }
                            if is_template {
                                let _ = inst.call_on_item_event(
                                    item_idx,
                                    event_id.0,
                                    propagation_item_idx,
                                );
                            } else {
                                let _ = inst.fire_event(event_id.0);
                            }
                        })
                    } else {
                        raw_el
                    };

                    raw_el
                })
                .into_raw_unchecked()
        }};
    }

    finish_item_text_input!(ti)
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
    let propagation_item_idx = ctx.propagation_item_idx;

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
        .map(|e| is_item_local_event(program, ctx, e))
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
                    let _ =
                        inst_change.call_on_item_event(item_idx, event_id.0, propagation_item_idx);
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
            | IrNode::StreamSkip { cell: c, .. }
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
            | IrNode::MathMin { cell: c, .. }
            | IrNode::MathMax { cell: c, .. }
            | IrNode::TextStartsWith { cell: c, .. }
            | IrNode::HoldLoop { cell: c, .. } => {
                if *c == cell {
                    return Some(node);
                }
            }
            IrNode::Document { root, .. } => {
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
                        store.get_cell_signal(cell_id).map(|v| format!("{}px", v)),
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
                        store.get_cell_signal(cell_id).map(|v| format!("{}px", v)),
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
fn compute_physical_gloss_background(gloss: f64, scene: PhysicalSceneParams) -> Option<String> {
    let gloss = gloss.clamp(0.0, 1.0);
    if gloss > 0.0 {
        let alpha = gloss * 0.25;
        Some(format!(
            "linear-gradient({:.0}deg,rgba(255,255,255,{alpha:.2}) 0%,transparent 50%,rgba(0,0,0,{:.2}) 100%)",
            scene.bevel_angle,
            alpha * 0.3
        ))
    } else {
        None
    }
}

fn compute_physical_depth_box_shadow(depth: f64, scene: PhysicalSceneParams) -> Option<String> {
    if depth <= 0.0 {
        return None;
    }

    let dx = depth * scene.shadow_dx_per_depth;
    let dy = depth * scene.shadow_dy_per_depth;
    let blur = depth * scene.shadow_blur_per_depth;
    let opacity = scene.shadow_opacity();
    let amb_blur = blur * 2.0;
    let amb_opacity = opacity * 0.4;
    Some(format!(
        "{dx:.1}px {dy:.1}px {blur:.1}px rgba(0,0,0,{opacity:.2}), 0px {:.1}px {amb_blur:.1}px rgba(0,0,0,{amb_opacity:.2})",
        dy * 0.5
    ))
}

fn apply_physical_material<T: RawEl>(
    mut el: T,
    value: &IrExpr,
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    item_ctx: Option<&ItemContext>,
) -> T {
    let scene_params = instance.scene_params.clone();
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
                    let gloss = *n;
                    el = el.style_signal(
                        "background-image",
                        scene_params
                            .signal()
                            .map(move |scene| compute_physical_gloss_background(gloss, scene)),
                    );
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
    let scene_params = instance.scene_params.clone();
    match value {
        IrExpr::Constant(IrValue::Number(depth)) => {
            let depth = *depth;
            el.style_signal(
                "box-shadow",
                scene_params
                    .signal()
                    .map(move |scene| compute_physical_depth_box_shadow(depth, scene)),
            )
        }
        IrExpr::CellRead(cell) => {
            let store = instance.cell_store.clone();
            let cell_id = cell.0;
            let scene_params = scene_params;
            let is_template = item_ctx.map_or(false, |ctx| {
                cell_id >= ctx.template_cell_range.0 && cell_id < ctx.template_cell_range.1
            });
            if is_template {
                if let Some(ctx) = item_ctx {
                    if let Some(ref ics) = instance.item_cell_store {
                        let signal = ics.get_signal(ctx.item_idx, cell_id);
                        return el.style_signal(
                            "box-shadow",
                            map_ref! {
                                let depth = signal,
                                let scene = scene_params.signal() => {
                                    compute_physical_depth_box_shadow(*depth, *scene)
                                        .or_else(|| Some("none".to_string()))
                                }
                            },
                        );
                    }
                }
            }
            el.style_signal(
                "box-shadow",
                map_ref! {
                    let depth = store.get_cell_signal(cell_id),
                    let scene = scene_params.signal() => {
                        compute_physical_depth_box_shadow(*depth, *scene)
                            .or_else(|| Some("none".to_string()))
                    }
                },
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
        IrExpr::Constant(IrValue::Tag(t)) if t == "Fully" => el.style("border-radius", "9999px"),
        _ => el,
    }
}

/// Apply spring_range as CSS transition.
fn apply_physical_spring_range<T: RawEl>(el: T, value: &IrExpr, program: &IrProgram) -> T {
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
                &format!("0 {wall}px 0 rgba(255,255,255,0.3), 0 -{wall}px 0 rgba(0,0,0,0.2)"),
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
            IrNode::PipeThrough { source, .. } => return resolve_to_object_store(program, *source),
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

fn resolve_runtime_expr(
    program: &IrProgram,
    cell_store: &CellStore,
    expr: &IrExpr,
    depth: u32,
) -> Option<IrExpr> {
    resolve_runtime_expr_with_bindings(program, cell_store, expr, depth, &HashMap::new())
}

fn resolve_runtime_object_fields_with_bindings(
    program: &IrProgram,
    cell_store: &CellStore,
    expr: &IrExpr,
    depth: u32,
    bindings: &HashMap<CellId, IrExpr>,
) -> Option<Vec<(String, IrExpr)>> {
    match resolve_runtime_expr_with_bindings(program, cell_store, expr, depth + 1, bindings)
        .unwrap_or_else(|| expr.clone())
    {
        IrExpr::ObjectConstruct(fields) => Some(fields),
        IrExpr::TaggedObject { fields, .. } => Some(fields),
        IrExpr::CellRead(cell) => {
            if let Some(bound) = bindings.get(&cell) {
                resolve_runtime_object_fields_with_bindings(
                    program,
                    cell_store,
                    bound,
                    depth + 1,
                    bindings,
                )
            } else {
                let object_store = resolve_to_object_store(program, cell);
                if let Some(bound) = bindings.get(&object_store) {
                    resolve_runtime_object_fields_with_bindings(
                        program,
                        cell_store,
                        bound,
                        depth + 1,
                        bindings,
                    )
                } else {
                    let field_map = program.cell_field_cells.get(&object_store)?;
                    let canonical_named_cells = canonical_named_cells(program);
                    let fields: Vec<(String, IrExpr)> = field_map
                        .iter()
                        .map(|(name, field_cell)| {
                            let effective_field_cell = effective_runtime_field_cell(
                                program,
                                &canonical_named_cells,
                                *field_cell,
                                bindings,
                            );
                            let value =
                                if program.cell_field_cells.contains_key(&effective_field_cell) {
                                    resolve_runtime_object_fields_with_bindings(
                                        program,
                                        cell_store,
                                        &IrExpr::CellRead(effective_field_cell),
                                        depth + 1,
                                        bindings,
                                    )
                                    .map(IrExpr::ObjectConstruct)
                                } else {
                                    None
                                }
                                .or_else(|| {
                                    find_node_for_cell(program, effective_field_cell).and_then(
                                        |node| match node {
                                            IrNode::Derived {
                                                expr: IrExpr::CellRead(source),
                                                ..
                                            } => bindings.get(source).and_then(|bound| {
                                                resolve_runtime_expr_with_bindings(
                                                    program,
                                                    cell_store,
                                                    bound,
                                                    depth + 1,
                                                    bindings,
                                                )
                                                .or_else(|| Some(bound.clone()))
                                            }),
                                            _ => None,
                                        },
                                    )
                                })
                                .or_else(|| {
                                    resolve_runtime_cell_expr_with_bindings(
                                        program,
                                        cell_store,
                                        effective_field_cell,
                                        depth + 1,
                                        bindings,
                                    )
                                })
                                .or_else(|| {
                                    find_node_for_cell(program, effective_field_cell).and_then(
                                        |node| match node {
                                            IrNode::Derived { expr, .. } => {
                                                resolve_runtime_expr_with_bindings(
                                                    program,
                                                    cell_store,
                                                    expr,
                                                    depth + 1,
                                                    bindings,
                                                )
                                                .or_else(|| Some(expr.clone()))
                                            }
                                            _ => None,
                                        },
                                    )
                                })
                                .unwrap_or_else(|| IrExpr::CellRead(effective_field_cell));
                            (name.clone(), value)
                        })
                        .collect();
                    (!fields.is_empty()).then_some(fields)
                }
            }
        }
        _ => None,
    }
}

fn resolve_bound_field_alias_cell(
    program: &IrProgram,
    cell_store: &CellStore,
    cell: CellId,
    depth: u32,
    bindings: &HashMap<CellId, IrExpr>,
) -> Option<IrExpr> {
    let cell_name = program.cells.get(cell.0 as usize)?.name.as_str();
    let object_prefix = cell_name
        .rsplit_once('.')
        .map(|(prefix, _)| prefix.to_string());
    let mut field_names = Vec::new();
    field_names.push(
        cell_name
            .rsplit('.')
            .next()
            .unwrap_or(cell_name)
            .to_string(),
    );
    if let Some(stem) = field_names[0].strip_suffix("_number") {
        field_names.push(stem.to_string());
    }

    for field_name in field_names {
        if let Some(prefix) = object_prefix.as_deref() {
            let mut matching_keys: Vec<CellId> = bindings
                .keys()
                .copied()
                .filter(|bound_cell| {
                    program
                        .cells
                        .get(bound_cell.0 as usize)
                        .map(|info| {
                            info.name == prefix || info.name.ends_with(&format!(".{prefix}"))
                        })
                        .unwrap_or(false)
                })
                .collect();
            matching_keys.sort_by_key(|cell| cell.0);

            for bound_cell in matching_keys {
                let Some(bound) = bindings.get(&bound_cell) else {
                    continue;
                };
                let fields = match bound {
                    IrExpr::ObjectConstruct(fields) | IrExpr::TaggedObject { fields, .. } => fields,
                    _ => continue,
                };
                if let Some(value) = fields
                    .iter()
                    .find_map(|(name, value)| (name == &field_name).then_some(value.clone()))
                {
                    if let Some(resolved) = resolve_runtime_expr_with_bindings(
                        program,
                        cell_store,
                        &value,
                        depth + 1,
                        bindings,
                    ) {
                        return Some(resolved);
                    }
                    return Some(value);
                }
            }
        }

        if object_prefix.is_none() {
            if let Some(value) = bindings.values().find_map(|bound| {
                let fields = match bound {
                    IrExpr::ObjectConstruct(fields) | IrExpr::TaggedObject { fields, .. } => fields,
                    _ => return None,
                };
                let value = fields
                    .iter()
                    .find_map(|(name, value)| (name == &field_name).then_some(value.clone()))?;
                resolve_runtime_expr_with_bindings(program, cell_store, &value, depth + 1, bindings)
                    .or(Some(value))
            }) {
                return Some(value);
            }
        }
    }

    None
}

fn normalize_runtime_item_expr_with_bindings(
    program: &IrProgram,
    cell_store: &CellStore,
    expr: &IrExpr,
    depth: u32,
    bindings: &HashMap<CellId, IrExpr>,
) -> IrExpr {
    fn normalize(
        program: &IrProgram,
        cell_store: &CellStore,
        expr: &IrExpr,
        depth: u32,
        bindings: &HashMap<CellId, IrExpr>,
    ) -> IrExpr {
        if depth > RUNTIME_RESOLVE_DEPTH_LIMIT {
            return expr.clone();
        }

        match expr {
            IrExpr::ObjectConstruct(fields) => IrExpr::ObjectConstruct(
                fields
                    .iter()
                    .map(|(name, value)| {
                        (
                            name.clone(),
                            normalize(program, cell_store, value, depth + 1, bindings),
                        )
                    })
                    .collect(),
            ),
            IrExpr::TaggedObject { tag, fields } => IrExpr::TaggedObject {
                tag: tag.clone(),
                fields: fields
                    .iter()
                    .map(|(name, value)| {
                        (
                            name.clone(),
                            normalize(program, cell_store, value, depth + 1, bindings),
                        )
                    })
                    .collect(),
            },
            IrExpr::ListConstruct(items) => IrExpr::ListConstruct(items.clone()),
            IrExpr::CellRead(cell) => {
                if let Some(bound) = bindings.get(cell) {
                    return normalize(program, cell_store, bound, depth + 1, bindings);
                }

                match find_node_for_cell(program, *cell) {
                    Some(IrNode::Derived { expr, .. }) => match expr {
                        IrExpr::ObjectConstruct(_)
                        | IrExpr::TaggedObject { .. }
                        | IrExpr::ListConstruct(_) => {
                            return normalize(program, cell_store, expr, depth + 1, bindings);
                        }
                        IrExpr::CellRead(source) => {
                            return normalize(
                                program,
                                cell_store,
                                &IrExpr::CellRead(*source),
                                depth + 1,
                                bindings,
                            );
                        }
                        _ => {}
                    },
                    Some(IrNode::PipeThrough { source, .. }) => {
                        return normalize(
                            program,
                            cell_store,
                            &IrExpr::CellRead(*source),
                            depth + 1,
                            bindings,
                        );
                    }
                    _ => {}
                }

                let resolved = resolve_runtime_expr_with_bindings(
                    program,
                    cell_store,
                    expr,
                    depth + 1,
                    bindings,
                )
                .unwrap_or_else(|| expr.clone());

                match resolved {
                    IrExpr::CellRead(cell) => resolve_runtime_object_fields_with_bindings(
                        program,
                        cell_store,
                        &IrExpr::CellRead(cell),
                        depth + 1,
                        bindings,
                    )
                    .map(IrExpr::ObjectConstruct)
                    .unwrap_or(IrExpr::CellRead(cell)),
                    IrExpr::ObjectConstruct(fields) => IrExpr::ObjectConstruct(
                        fields
                            .iter()
                            .map(|(name, value)| {
                                (
                                    name.clone(),
                                    normalize(program, cell_store, value, depth + 1, bindings),
                                )
                            })
                            .collect(),
                    ),
                    IrExpr::TaggedObject { tag, fields } => IrExpr::TaggedObject {
                        tag,
                        fields: fields
                            .iter()
                            .map(|(name, value)| {
                                (
                                    name.clone(),
                                    normalize(program, cell_store, value, depth + 1, bindings),
                                )
                            })
                            .collect(),
                    },
                    IrExpr::ListConstruct(items) => IrExpr::ListConstruct(items),
                    other => other,
                }
            }
            _ => resolve_runtime_expr_with_bindings(program, cell_store, expr, depth + 1, bindings)
                .unwrap_or_else(|| expr.clone()),
        }
    }

    normalize(program, cell_store, expr, depth + 1, bindings)
}

fn resolve_runtime_expr_with_bindings(
    program: &IrProgram,
    cell_store: &CellStore,
    expr: &IrExpr,
    depth: u32,
    bindings: &HashMap<CellId, IrExpr>,
) -> Option<IrExpr> {
    if depth > RUNTIME_RESOLVE_DEPTH_LIMIT {
        return None;
    }

    match expr {
        IrExpr::Constant(_) => Some(expr.clone()),
        IrExpr::ObjectConstruct(fields) => Some(IrExpr::ObjectConstruct(
            fields
                .iter()
                .map(|(name, value)| {
                    (
                        name.clone(),
                        resolve_runtime_expr_with_bindings(
                            program,
                            cell_store,
                            value,
                            depth + 1,
                            bindings,
                        )
                        .unwrap_or_else(|| value.clone()),
                    )
                })
                .collect(),
        )),
        IrExpr::ListConstruct(items) => Some(IrExpr::ListConstruct(
            items
                .iter()
                .map(|item| {
                    resolve_runtime_expr_with_bindings(
                        program,
                        cell_store,
                        item,
                        depth + 1,
                        bindings,
                    )
                    .unwrap_or_else(|| item.clone())
                })
                .collect(),
        )),
        IrExpr::TaggedObject { tag, fields } => Some(IrExpr::TaggedObject {
            tag: tag.clone(),
            fields: fields
                .iter()
                .map(|(name, value)| {
                    (
                        name.clone(),
                        resolve_runtime_expr_with_bindings(
                            program,
                            cell_store,
                            value,
                            depth + 1,
                            bindings,
                        )
                        .unwrap_or_else(|| value.clone()),
                    )
                })
                .collect(),
        }),
        IrExpr::CellRead(cell) => {
            if let Some(bound) = bindings.get(cell) {
                resolve_runtime_expr_with_bindings(program, cell_store, bound, depth + 1, bindings)
                    .or_else(|| Some(bound.clone()))
            } else {
                resolve_runtime_cell_expr_with_bindings(
                    program,
                    cell_store,
                    *cell,
                    depth + 1,
                    bindings,
                )
            }
        }
        IrExpr::FieldAccess { object, field } => {
            let resolve_field_value = |value: IrExpr| match value {
                IrExpr::ListConstruct(_)
                | IrExpr::ObjectConstruct(_)
                | IrExpr::TaggedObject { .. } => Some(value),
                other => resolve_runtime_expr_with_bindings(
                    program,
                    cell_store,
                    &other,
                    depth + 1,
                    bindings,
                )
                .or(Some(other)),
            };

            let field_from_bound = |bound: &IrExpr| match bound {
                IrExpr::ObjectConstruct(fields) | IrExpr::TaggedObject { fields, .. } => fields
                    .iter()
                    .find_map(|(name, value)| (name == field).then_some(value.clone())),
                _ => None,
            };

            if let IrExpr::CellRead(object_cell) = &**object {
                let object_store = resolve_to_object_store(program, *object_cell);
                if let Some(value) = bindings
                    .get(object_cell)
                    .and_then(field_from_bound)
                    .or_else(|| bindings.get(&object_store).and_then(field_from_bound))
                {
                    return resolve_field_value(value);
                }

                if let Some(field_cell) = program
                    .cell_field_cells
                    .get(&object_store)
                    .and_then(|fields| fields.get(field))
                {
                    let canonical_named_cells = canonical_named_cells(program);
                    let effective_field_cell = effective_runtime_field_cell(
                        program,
                        &canonical_named_cells,
                        *field_cell,
                        bindings,
                    );
                    return resolve_runtime_cell_expr_with_bindings(
                        program,
                        cell_store,
                        effective_field_cell,
                        depth + 1,
                        bindings,
                    )
                    .or_else(|| Some(IrExpr::CellRead(effective_field_cell)));
                }
            }

            resolve_runtime_object_fields_with_bindings(
                program,
                cell_store,
                object,
                depth + 1,
                bindings,
            )
            .and_then(|fields| {
                fields
                    .into_iter()
                    .find(|(name, _)| name == field)
                    .and_then(|(_, value)| resolve_field_value(value))
            })
        }
        IrExpr::BinOp { op, lhs, rhs } => {
            let lhs =
                resolve_runtime_expr_with_bindings(program, cell_store, lhs, depth + 1, bindings)
                    .unwrap_or_else(|| *lhs.clone());
            let rhs =
                resolve_runtime_expr_with_bindings(program, cell_store, rhs, depth + 1, bindings)
                    .unwrap_or_else(|| *rhs.clone());
            match (lhs, rhs) {
                (IrExpr::Constant(IrValue::Number(a)), IrExpr::Constant(IrValue::Number(b))) => {
                    let value = match op {
                        BinOp::Add => a + b,
                        BinOp::Sub => a - b,
                        BinOp::Mul => a * b,
                        BinOp::Div => a / b,
                    };
                    Some(IrExpr::Constant(IrValue::Number(value)))
                }
                _ => None,
            }
        }
        IrExpr::Compare { op, lhs, rhs } => {
            let lhs_resolved =
                resolve_runtime_expr_with_bindings(program, cell_store, lhs, depth + 1, bindings)
                    .unwrap_or_else(|| *lhs.clone());
            let rhs_resolved =
                resolve_runtime_expr_with_bindings(program, cell_store, rhs, depth + 1, bindings)
                    .unwrap_or_else(|| *rhs.clone());

            let lhs_number =
                resolve_runtime_number_with_bindings(program, cell_store, lhs, depth + 1, bindings)
                    .or(match &lhs_resolved {
                        IrExpr::Constant(IrValue::Number(number)) => Some(*number),
                        IrExpr::Constant(IrValue::Bool(value)) => {
                            Some(if *value { 1.0 } else { 0.0 })
                        }
                        IrExpr::CellRead(cell) => {
                            let value = cell_store.get_cell_value(cell.0);
                            (!value.is_nan()).then_some(value)
                        }
                        _ => None,
                    });
            let rhs_number =
                resolve_runtime_number_with_bindings(program, cell_store, rhs, depth + 1, bindings)
                    .or(match &rhs_resolved {
                        IrExpr::Constant(IrValue::Number(number)) => Some(*number),
                        IrExpr::Constant(IrValue::Bool(value)) => {
                            Some(if *value { 1.0 } else { 0.0 })
                        }
                        IrExpr::CellRead(cell) => {
                            let value = cell_store.get_cell_value(cell.0);
                            (!value.is_nan()).then_some(value)
                        }
                        _ => None,
                    });

            if let (Some(a), Some(b)) = (lhs_number, rhs_number) {
                let value = match op {
                    CmpOp::Eq => a == b,
                    CmpOp::Ne => a != b,
                    CmpOp::Gt => a > b,
                    CmpOp::Ge => a >= b,
                    CmpOp::Lt => a < b,
                    CmpOp::Le => a <= b,
                };
                return Some(IrExpr::Constant(IrValue::Number(if value {
                    1.0
                } else {
                    0.0
                })));
            }

            let lhs_text =
                resolve_runtime_text_with_bindings(program, cell_store, lhs, depth + 1, bindings);
            let rhs_text =
                resolve_runtime_text_with_bindings(program, cell_store, rhs, depth + 1, bindings);
            match (lhs_text, rhs_text) {
                (Some(a), Some(b)) => {
                    let value = match op {
                        CmpOp::Eq => a == b,
                        CmpOp::Ne => a != b,
                        CmpOp::Gt => a > b,
                        CmpOp::Ge => a >= b,
                        CmpOp::Lt => a < b,
                        CmpOp::Le => a <= b,
                    };
                    Some(IrExpr::Constant(IrValue::Number(if value {
                        1.0
                    } else {
                        0.0
                    })))
                }
                _ => None,
            }
        }
        IrExpr::UnaryNeg(inner) => {
            match resolve_runtime_expr_with_bindings(
                program,
                cell_store,
                inner,
                depth + 1,
                bindings,
            )
            .unwrap_or_else(|| *inner.clone())
            {
                IrExpr::Constant(IrValue::Number(n)) => Some(IrExpr::Constant(IrValue::Number(-n))),
                _ => None,
            }
        }
        IrExpr::Not(inner) => {
            match resolve_runtime_expr_with_bindings(
                program,
                cell_store,
                inner,
                depth + 1,
                bindings,
            )
            .unwrap_or_else(|| *inner.clone())
            {
                IrExpr::Constant(IrValue::Number(n)) => {
                    Some(IrExpr::Constant(IrValue::Number(if n == 0.0 {
                        1.0
                    } else {
                        0.0
                    })))
                }
                _ => None,
            }
        }
        IrExpr::TextConcat(parts) => {
            let mut text = String::new();
            for part in parts {
                match part {
                    TextSegment::Literal(t) => text.push_str(t),
                    TextSegment::Expr(expr) => match resolve_runtime_expr_with_bindings(
                        program,
                        cell_store,
                        expr,
                        depth + 1,
                        bindings,
                    )
                    .unwrap_or_else(|| expr.clone())
                    {
                        IrExpr::Constant(IrValue::Text(t)) => text.push_str(&t),
                        IrExpr::Constant(IrValue::Number(n)) => text.push_str(&format_number(n)),
                        IrExpr::Constant(IrValue::Tag(t)) => text.push_str(&t),
                        IrExpr::CellRead(cell) => {
                            text.push_str(&format_runtime_cell_value_with_bindings(
                                program, cell_store, cell, bindings,
                            ))
                        }
                        _ => {}
                    },
                }
            }
            Some(IrExpr::Constant(IrValue::Text(text)))
        }
        IrExpr::FunctionCall { func, args } => {
            let ir_func = program.functions.get(func.0 as usize)?;
            let mut nested_bindings = bindings.clone();
            for (param_cell, arg) in ir_func.param_cells.iter().zip(args.iter()) {
                let resolved_arg = resolve_runtime_expr_with_bindings(
                    program,
                    cell_store,
                    arg,
                    depth + 1,
                    bindings,
                )
                .unwrap_or_else(|| arg.clone());
                nested_bindings.insert(*param_cell, resolved_arg);
            }
            resolve_runtime_expr_with_bindings(
                program,
                cell_store,
                &ir_func.body,
                depth + 1,
                &nested_bindings,
            )
            .or_else(|| Some(ir_func.body.clone()))
        }
        IrExpr::PatternMatch { source, arms } => {
            let matchers: Vec<ArmMatcher> = arms
                .iter()
                .map(|(pattern, _)| pattern_to_matcher(pattern, &program.tag_table))
                .collect();
            let source_expr = resolve_runtime_cell_expr_with_bindings(
                program,
                cell_store,
                *source,
                depth + 1,
                bindings,
            )
            .unwrap_or(IrExpr::CellRead(*source));
            let (value, text) = match source_expr {
                IrExpr::Constant(IrValue::Number(n)) => (n, String::new()),
                IrExpr::Constant(IrValue::Bool(b)) => (if b { 1.0 } else { 0.0 }, String::new()),
                IrExpr::Constant(IrValue::Text(t)) => (f64::NAN, t),
                IrExpr::Constant(IrValue::Tag(t)) => {
                    (encoded_runtime_tag_value(&program.tag_table, &t), t)
                }
                IrExpr::CellRead(cell) => (
                    cell_store.get_cell_value(cell.0),
                    cell_store.get_cell_text(cell.0),
                ),
                _ => (
                    cell_store.get_cell_value(source.0),
                    cell_store.get_cell_text(source.0),
                ),
            };
            let arm = find_matching_arm_idx(&matchers, value, &text)?;
            resolve_runtime_expr_with_bindings(
                program,
                cell_store,
                &arms[arm].1,
                depth + 1,
                bindings,
            )
            .or_else(|| Some(arms[arm].1.clone()))
        }
    }
}

fn resolve_runtime_text_with_bindings(
    program: &IrProgram,
    cell_store: &CellStore,
    expr: &IrExpr,
    depth: u32,
    bindings: &HashMap<CellId, IrExpr>,
) -> Option<String> {
    match resolve_runtime_expr_with_bindings(program, cell_store, expr, depth + 1, bindings)
        .unwrap_or_else(|| expr.clone())
    {
        IrExpr::Constant(IrValue::Text(text)) => Some(text),
        IrExpr::Constant(IrValue::Tag(tag)) => visible_tag_text(&tag),
        IrExpr::Constant(IrValue::Number(number)) => Some(format_number(number)),
        IrExpr::CellRead(cell) => {
            let text = cell_store.get_cell_text(cell.0);
            if !text.is_empty() {
                Some(text)
            } else {
                let value = cell_store.get_cell_value(cell.0);
                (!value.is_nan()).then(|| format_number(value))
            }
        }
        _ => None,
    }
}

fn resolve_runtime_number_with_bindings(
    program: &IrProgram,
    cell_store: &CellStore,
    expr: &IrExpr,
    depth: u32,
    bindings: &HashMap<CellId, IrExpr>,
) -> Option<f64> {
    match resolve_runtime_expr_with_bindings(program, cell_store, expr, depth + 1, bindings)
        .unwrap_or_else(|| expr.clone())
    {
        IrExpr::Constant(IrValue::Number(number)) => Some(number),
        IrExpr::CellRead(cell) => {
            let value = cell_store.get_cell_value(cell.0);
            (!value.is_nan()).then_some(value)
        }
        _ => None,
    }
}

fn resolve_runtime_cell_expr(
    program: &IrProgram,
    cell_store: &CellStore,
    cell: CellId,
    depth: u32,
) -> Option<IrExpr> {
    resolve_runtime_cell_expr_with_bindings(program, cell_store, cell, depth, &HashMap::new())
}

fn resolve_runtime_cell_expr_with_bindings(
    program: &IrProgram,
    cell_store: &CellStore,
    cell: CellId,
    depth: u32,
    bindings: &HashMap<CellId, IrExpr>,
) -> Option<IrExpr> {
    if depth > RUNTIME_RESOLVE_DEPTH_LIMIT {
        return None;
    }

    if let Some(bound) = bindings.get(&cell) {
        return resolve_runtime_expr_with_bindings(program, cell_store, bound, depth + 1, bindings)
            .or_else(|| Some(bound.clone()));
    }

    let Some(node) = find_node_for_cell(program, cell) else {
        return resolve_bound_field_alias_cell(program, cell_store, cell, depth + 1, bindings);
    };

    let prefers_live_store = matches!(
        node,
        IrNode::Derived {
            expr: IrExpr::Constant(_) | IrExpr::CellRead(_),
            ..
        } | IrNode::PipeThrough { .. }
    );
    let is_template_cell = program.nodes.iter().any(|node| match node {
        IrNode::ListMap {
            template_cell_range,
            ..
        } => cell.0 >= template_cell_range.0 && cell.0 < template_cell_range.1,
        _ => false,
    });
    if prefers_live_store
        && !program.cell_field_cells.contains_key(&cell)
        && !(is_template_cell && !bindings.is_empty())
    {
        let text = cell_store.get_cell_text(cell.0);
        if !text.is_empty() {
            return Some(IrExpr::Constant(IrValue::Text(text)));
        }
        let value = cell_store.get_cell_value(cell.0);
        if !value.is_nan() {
            return Some(IrExpr::Constant(IrValue::Number(value)));
        }
    }
    match node {
        IrNode::Derived {
            expr: IrExpr::CellRead(source),
            ..
        } => {
            return resolve_runtime_cell_expr_with_bindings(
                program,
                cell_store,
                *source,
                depth + 1,
                bindings,
            );
        }
        IrNode::PipeThrough { source, .. } => {
            return resolve_runtime_cell_expr_with_bindings(
                program,
                cell_store,
                *source,
                depth + 1,
                bindings,
            );
        }
        _ => {}
    }

    let object_store = resolve_to_object_store(program, cell);
    let is_list_like_node = matches!(
        node,
        IrNode::Derived {
            expr: IrExpr::ListConstruct(_),
            ..
        } | IrNode::ListMap { .. }
            | IrNode::ListAppend { .. }
            | IrNode::ListClear { .. }
            | IrNode::ListCount { .. }
            | IrNode::ListRemove { .. }
            | IrNode::ListRetain { .. }
            | IrNode::ListEvery { .. }
            | IrNode::ListAny { .. }
            | IrNode::ListIsEmpty { .. }
    );
    if object_store == cell
        && program.cell_field_cells.contains_key(&object_store)
        && !is_list_like_node
    {
        if let Some(fields) = resolve_runtime_object_fields_with_bindings(
            program,
            cell_store,
            &IrExpr::CellRead(object_store),
            depth + 1,
            bindings,
        ) {
            return Some(IrExpr::ObjectConstruct(fields));
        }
        return Some(IrExpr::ObjectConstruct(reconstruct_object_fields(
            program,
            object_store,
        )));
    }

    match node {
        IrNode::Derived { expr, .. } => {
            resolve_runtime_expr_with_bindings(program, cell_store, expr, depth + 1, bindings)
                .or_else(|| Some(expr.clone()))
        }
        IrNode::Hold { init, .. } => {
            let text = cell_store.get_cell_text(cell.0);
            if !text.is_empty() {
                Some(IrExpr::Constant(IrValue::Text(text)))
            } else {
                let value = cell_store.get_cell_value(cell.0);
                if !value.is_nan() {
                    Some(IrExpr::Constant(IrValue::Number(value)))
                } else {
                    resolve_runtime_expr_with_bindings(
                        program,
                        cell_store,
                        init,
                        depth + 1,
                        bindings,
                    )
                    .or_else(|| Some(init.clone()))
                }
            }
        }
        IrNode::Latest { arms, .. } => {
            let mut fallback = None;
            let mut best = None;
            let mut best_score = 0u8;
            for arm in arms {
                let resolved = resolve_runtime_expr_with_bindings(
                    program,
                    cell_store,
                    &arm.body,
                    depth + 1,
                    bindings,
                )
                .or_else(|| Some(arm.body.clone()));

                let Some(expr) = resolved else {
                    continue;
                };
                if matches!(expr, IrExpr::Constant(IrValue::Skip)) {
                    continue;
                }
                if fallback.is_none() {
                    fallback = Some(expr.clone());
                }
                let score = runtime_resolve_score(&expr);
                if score >= best_score && score > 0 {
                    best_score = score;
                    best = Some(expr);
                }
            }
            best.or(fallback)
        }
        IrNode::When { source, arms, .. } | IrNode::While { source, arms, .. } => {
            let matchers: Vec<ArmMatcher> = arms
                .iter()
                .map(|(pattern, _)| pattern_to_matcher(pattern, &program.tag_table))
                .collect();
            let source_expr = resolve_runtime_cell_expr_with_bindings(
                program,
                cell_store,
                *source,
                depth + 1,
                bindings,
            )
            .unwrap_or(IrExpr::CellRead(*source));
            let (value, text) = match source_expr {
                IrExpr::Constant(IrValue::Number(n)) => (n, String::new()),
                IrExpr::Constant(IrValue::Bool(b)) => (if b { 1.0 } else { 0.0 }, String::new()),
                IrExpr::Constant(IrValue::Text(t)) => (f64::NAN, t),
                IrExpr::Constant(IrValue::Tag(t)) => {
                    (encoded_runtime_tag_value(&program.tag_table, &t), t)
                }
                IrExpr::CellRead(cell) => (
                    cell_store.get_cell_value(cell.0),
                    cell_store.get_cell_text(cell.0),
                ),
                _ => (
                    cell_store.get_cell_value(source.0),
                    cell_store.get_cell_text(source.0),
                ),
            };
            let arm = find_matching_arm_idx(&matchers, value, &text)?;
            resolve_runtime_expr_with_bindings(
                program,
                cell_store,
                &arms[arm].1,
                depth + 1,
                bindings,
            )
            .or_else(|| Some(arms[arm].1.clone()))
        }
        IrNode::TextTrim { source, .. } => {
            let text = resolve_runtime_text_with_bindings(
                program,
                cell_store,
                &IrExpr::CellRead(*source),
                depth + 1,
                bindings,
            )?;
            Some(IrExpr::Constant(IrValue::Text(text.trim().to_string())))
        }
        IrNode::TextIsNotEmpty { source, .. } => {
            let text = resolve_runtime_text_with_bindings(
                program,
                cell_store,
                &IrExpr::CellRead(*source),
                depth + 1,
                bindings,
            )
            .unwrap_or_default();
            Some(IrExpr::Constant(IrValue::Number(if text.is_empty() {
                0.0
            } else {
                1.0
            })))
        }
        IrNode::TextToNumber {
            source,
            nan_tag_value,
            ..
        } => {
            let text = resolve_runtime_text_with_bindings(
                program,
                cell_store,
                &IrExpr::CellRead(*source),
                depth + 1,
                bindings,
            )
            .unwrap_or_default();
            let trimmed = text.trim();
            let parsed = if trimmed.is_empty() {
                None
            } else {
                trimmed
                    .parse::<f64>()
                    .ok()
                    .filter(|value| value.is_finite())
            };
            Some(match parsed {
                Some(number) => IrExpr::Constant(IrValue::Number(number)),
                None => {
                    let tag_name = if *nan_tag_value > 0.0 {
                        program
                            .tag_table
                            .get(nan_tag_value.round() as usize - 1)
                            .cloned()
                            .unwrap_or_else(|| "NaN".to_string())
                    } else {
                        "NaN".to_string()
                    };
                    IrExpr::Constant(IrValue::Tag(tag_name))
                }
            })
        }
        IrNode::TextStartsWith { source, prefix, .. } => {
            let source_text = resolve_runtime_text_with_bindings(
                program,
                cell_store,
                &IrExpr::CellRead(*source),
                depth + 1,
                bindings,
            )
            .unwrap_or_default();
            let prefix_text = resolve_runtime_text_with_bindings(
                program,
                cell_store,
                &IrExpr::CellRead(*prefix),
                depth + 1,
                bindings,
            )
            .unwrap_or_default();
            Some(IrExpr::Constant(IrValue::Number(
                if source_text.starts_with(&prefix_text) {
                    1.0
                } else {
                    0.0
                },
            )))
        }
        IrNode::MathRound { source, .. } => {
            let number = resolve_runtime_number_with_bindings(
                program,
                cell_store,
                &IrExpr::CellRead(*source),
                depth + 1,
                bindings,
            )?;
            Some(IrExpr::Constant(IrValue::Number(number.round())))
        }
        IrNode::MathMin { source, b, .. } => {
            let source_value = resolve_runtime_number_with_bindings(
                program,
                cell_store,
                &IrExpr::CellRead(*source),
                depth + 1,
                bindings,
            )?;
            let b_value = resolve_runtime_number_with_bindings(
                program,
                cell_store,
                &IrExpr::CellRead(*b),
                depth + 1,
                bindings,
            )?;
            Some(IrExpr::Constant(IrValue::Number(source_value.min(b_value))))
        }
        IrNode::MathMax { source, b, .. } => {
            let source_value = resolve_runtime_number_with_bindings(
                program,
                cell_store,
                &IrExpr::CellRead(*source),
                depth + 1,
                bindings,
            )?;
            let b_value = resolve_runtime_number_with_bindings(
                program,
                cell_store,
                &IrExpr::CellRead(*b),
                depth + 1,
                bindings,
            )?;
            Some(IrExpr::Constant(IrValue::Number(source_value.max(b_value))))
        }
        IrNode::CustomCall { path, args, .. }
            if path.len() == 2 && path[0] == "Text" && path[1] == "is_empty" =>
        {
            let source_expr = args
                .iter()
                .find(|(name, _)| name == "__source")
                .map(|(_, expr)| expr)?;
            let text = resolve_runtime_text_with_bindings(
                program,
                cell_store,
                source_expr,
                depth + 1,
                bindings,
            )
            .unwrap_or_default();
            Some(IrExpr::Constant(IrValue::Number(if text.is_empty() {
                1.0
            } else {
                0.0
            })))
        }
        IrNode::CustomCall { path, args, .. }
            if path.len() == 2 && path[0] == "Text" && path[1] == "is_not_empty" =>
        {
            let source_expr = args
                .iter()
                .find(|(name, _)| name == "__source")
                .map(|(_, expr)| expr)?;
            let text = resolve_runtime_text_with_bindings(
                program,
                cell_store,
                source_expr,
                depth + 1,
                bindings,
            )
            .unwrap_or_default();
            Some(IrExpr::Constant(IrValue::Number(if text.is_empty() {
                0.0
            } else {
                1.0
            })))
        }
        IrNode::CustomCall { path, args, .. }
            if path.len() == 2 && path[0] == "Text" && path[1] == "length" =>
        {
            let source_expr = args
                .iter()
                .find(|(name, _)| name == "__source")
                .map(|(_, expr)| expr)?;
            let text = resolve_runtime_text_with_bindings(
                program,
                cell_store,
                source_expr,
                depth + 1,
                bindings,
            )
            .unwrap_or_default();
            Some(IrExpr::Constant(IrValue::Number(
                text.chars().count() as f64
            )))
        }
        IrNode::CustomCall { path, args, .. }
            if path.len() == 2 && path[0] == "Text" && path[1] == "find" =>
        {
            let source_expr = args
                .iter()
                .find(|(name, _)| name == "__source")
                .map(|(_, expr)| expr)?;
            let search_expr = args
                .iter()
                .find(|(name, _)| name == "search")
                .map(|(_, expr)| expr)?;
            let source_text = resolve_runtime_text_with_bindings(
                program,
                cell_store,
                source_expr,
                depth + 1,
                bindings,
            )
            .unwrap_or_default();
            let search_text = resolve_runtime_text_with_bindings(
                program,
                cell_store,
                search_expr,
                depth + 1,
                bindings,
            )
            .unwrap_or_default();
            let index = source_text
                .find(&search_text)
                .map(|idx| idx as f64)
                .unwrap_or(-1.0);
            Some(IrExpr::Constant(IrValue::Number(index)))
        }
        IrNode::CustomCall { path, args, .. }
            if path.len() == 2 && path[0] == "Text" && path[1] == "substring" =>
        {
            let source_expr = args
                .iter()
                .find(|(name, _)| name == "__source")
                .map(|(_, expr)| expr)?;
            let start_expr = args
                .iter()
                .find(|(name, _)| name == "start")
                .map(|(_, expr)| expr)?;
            let length_expr = args
                .iter()
                .find(|(name, _)| name == "length")
                .map(|(_, expr)| expr)?;
            let source_text = resolve_runtime_text_with_bindings(
                program,
                cell_store,
                source_expr,
                depth + 1,
                bindings,
            )
            .unwrap_or_default();
            let start = resolve_runtime_number_with_bindings(
                program,
                cell_store,
                start_expr,
                depth + 1,
                bindings,
            )
            .unwrap_or(0.0)
            .max(0.0) as usize;
            let length = resolve_runtime_number_with_bindings(
                program,
                cell_store,
                length_expr,
                depth + 1,
                bindings,
            )
            .unwrap_or(0.0)
            .max(0.0) as usize;
            let text: String = source_text.chars().skip(start).take(length).collect();
            Some(IrExpr::Constant(IrValue::Text(text)))
        }
        _ => Some(IrExpr::CellRead(cell)),
    }
}

fn runtime_resolve_score(expr: &IrExpr) -> u8 {
    match expr {
        IrExpr::Constant(IrValue::Skip) => 0,
        IrExpr::Constant(_) => 4,
        IrExpr::ObjectConstruct(fields) => {
            if fields
                .iter()
                .all(|(_, value)| runtime_resolve_score(value) >= 3)
            {
                4
            } else {
                2
            }
        }
        IrExpr::ListConstruct(items) => {
            if items.iter().all(|item| runtime_resolve_score(item) >= 3) {
                4
            } else {
                2
            }
        }
        IrExpr::TaggedObject { fields, .. } => {
            if fields
                .iter()
                .all(|(_, value)| runtime_resolve_score(value) >= 3)
            {
                4
            } else {
                2
            }
        }
        IrExpr::CellRead(_) => 1,
        IrExpr::FieldAccess { .. }
        | IrExpr::FunctionCall { .. }
        | IrExpr::PatternMatch { .. }
        | IrExpr::BinOp { .. }
        | IrExpr::Compare { .. }
        | IrExpr::UnaryNeg(_)
        | IrExpr::Not(_)
        | IrExpr::TextConcat(_) => 2,
    }
}

fn resolve_runtime_list_item_expr(
    program: &IrProgram,
    cell_store: &CellStore,
    list_cell: CellId,
    index: usize,
) -> Option<IrExpr> {
    fn inner(
        program: &IrProgram,
        cell_store: &CellStore,
        list_cell: CellId,
        index: usize,
        seen: &mut HashSet<CellId>,
    ) -> Option<IrExpr> {
        if !seen.insert(list_cell) {
            return None;
        }
        let resolved = match resolve_runtime_cell_expr(program, cell_store, list_cell, 0)
            .unwrap_or(IrExpr::CellRead(list_cell))
        {
            IrExpr::ListConstruct(items) => items.get(index).map(|expr| {
                normalize_runtime_item_expr_with_bindings(
                    program,
                    cell_store,
                    expr,
                    1,
                    &HashMap::new(),
                )
            }),
            IrExpr::CellRead(next) if next != list_cell => {
                inner(program, cell_store, next, index, seen)
            }
            IrExpr::CellRead(cell) => match find_node_for_cell(program, cell) {
                Some(IrNode::Derived {
                    expr: IrExpr::ListConstruct(items),
                    ..
                }) => items.get(index).cloned(),
                Some(IrNode::ListMap {
                    cell,
                    source,
                    item_cell,
                    template,
                    ..
                }) => {
                    let source_item = inner(program, cell_store, *source, index, seen)?;
                    let list_map_plan = program.list_map_plan(*cell)?;
                    let resolved_item_store = list_map_plan.resolved_item_store;
                    let template_root = list_map_plan
                        .template
                        .root_cell
                        .filter(|root| *root != *item_cell && *root != resolved_item_store)
                        .or_else(|| node_output_cell(template))
                        .unwrap_or(resolved_item_store);
                    let bindings =
                        item_bindings(program, *item_cell, resolved_item_store, &source_item);
                    let mapped_item = if find_node_for_cell(program, template_root).is_none() {
                        resolve_inline_template_expr_with_bindings(
                            program, cell_store, template, 1, &bindings,
                        )
                    } else {
                        resolve_runtime_cell_expr_with_bindings(
                            program,
                            cell_store,
                            template_root,
                            1,
                            &bindings,
                        )
                        .or_else(|| {
                            resolve_runtime_object_fields_with_bindings(
                                program,
                                cell_store,
                                &IrExpr::CellRead(template_root),
                                1,
                                &bindings,
                            )
                            .map(IrExpr::ObjectConstruct)
                        })
                        .or_else(|| Some(IrExpr::CellRead(template_root)))
                    };

                    match mapped_item {
                        Some(IrExpr::CellRead(cell))
                            if resolve_runtime_object_fields_with_bindings(
                                program,
                                cell_store,
                                &IrExpr::CellRead(cell),
                                1,
                                &bindings,
                            )
                            .is_none() =>
                        {
                            Some(normalize_runtime_item_expr_with_bindings(
                                program,
                                cell_store,
                                &source_item,
                                1,
                                &bindings,
                            ))
                        }
                        Some(expr) => Some(normalize_runtime_item_expr_with_bindings(
                            program, cell_store, &expr, 1, &bindings,
                        )),
                        None => Some(normalize_runtime_item_expr_with_bindings(
                            program,
                            cell_store,
                            &source_item,
                            1,
                            &bindings,
                        )),
                    }
                }
                Some(IrNode::PipeThrough { source, .. }) => {
                    inner(program, cell_store, *source, index, seen)
                }
                _ => None,
            },
            _ => None,
        };
        seen.remove(&list_cell);
        resolved
    }

    inner(program, cell_store, list_cell, index, &mut HashSet::new())
}

fn resolve_runtime_list_item_expr_for_context(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    parent_item_ctx: Option<&ItemContext>,
    list_cell: CellId,
    index: usize,
) -> Option<IrExpr> {
    if let Some(parent_ctx) = parent_item_ctx {
        if parent_ctx.is_template_cell(list_cell) {
            let source_cell = program.list_map_plan(list_cell).map(|plan| plan.source);
            if let Some(list_id) = resolve_parent_template_list_id(
                program,
                &instance.cell_store,
                &instance.list_store,
                &instance.template_list_items,
                instance.item_cell_store.as_ref(),
                parent_ctx,
                list_cell,
                source_cell,
            ) {
                if let Some(items) = instance
                    .template_list_items
                    .borrow()
                    .get(&list_id.to_bits())
                {
                    if let Some(item) = items.get(index) {
                        if let Some(parent_item_expr) = parent_ctx.item_expr.as_ref() {
                            let bindings = item_bindings(
                                program,
                                CellId(parent_ctx.item_cell_id),
                                CellId(parent_ctx.resolved_item_store_id),
                                parent_item_expr,
                            );
                            return Some(normalize_runtime_item_expr_with_bindings(
                                program,
                                &instance.cell_store,
                                item,
                                0,
                                &bindings,
                            ));
                        }
                        return Some(item.clone());
                    }
                }
            }
        }
    }
    resolve_runtime_list_item_expr(program, &instance.cell_store, list_cell, index)
}

fn collect_expr_cell_reads(expr: &IrExpr, cells: &mut HashSet<CellId>) {
    match expr {
        IrExpr::CellRead(cell) => {
            cells.insert(*cell);
        }
        IrExpr::FieldAccess { object, .. } => collect_expr_cell_reads(object, cells),
        IrExpr::BinOp { lhs, rhs, .. } | IrExpr::Compare { lhs, rhs, .. } => {
            collect_expr_cell_reads(lhs, cells);
            collect_expr_cell_reads(rhs, cells);
        }
        IrExpr::UnaryNeg(inner) | IrExpr::Not(inner) => collect_expr_cell_reads(inner, cells),
        IrExpr::TextConcat(parts) => {
            for part in parts {
                if let TextSegment::Expr(expr) = part {
                    collect_expr_cell_reads(expr, cells);
                }
            }
        }
        IrExpr::FunctionCall { args, .. } | IrExpr::ListConstruct(args) => {
            for arg in args {
                collect_expr_cell_reads(arg, cells);
            }
        }
        IrExpr::ObjectConstruct(fields) | IrExpr::TaggedObject { fields, .. } => {
            for (_, value) in fields {
                collect_expr_cell_reads(value, cells);
            }
        }
        IrExpr::PatternMatch { source, arms } => {
            cells.insert(*source);
            for (_, body) in arms {
                collect_expr_cell_reads(body, cells);
            }
        }
        IrExpr::Constant(_) => {}
    }
}

fn collect_expr_seed_cells(program: &IrProgram, expr: &IrExpr, cells: &mut HashSet<CellId>) {
    match expr {
        IrExpr::CellRead(cell) => {
            cells.insert(*cell);
        }
        IrExpr::FieldAccess { object, field } => {
            if let IrExpr::CellRead(object_cell) = object.as_ref() {
                if let Some(field_cell) = program
                    .cell_field_cells
                    .get(object_cell)
                    .and_then(|fields| fields.get(field))
                {
                    cells.insert(*field_cell);
                }
            }
            collect_expr_seed_cells(program, object, cells);
        }
        IrExpr::BinOp { lhs, rhs, .. } | IrExpr::Compare { lhs, rhs, .. } => {
            collect_expr_seed_cells(program, lhs, cells);
            collect_expr_seed_cells(program, rhs, cells);
        }
        IrExpr::UnaryNeg(inner) | IrExpr::Not(inner) => {
            collect_expr_seed_cells(program, inner, cells)
        }
        IrExpr::TextConcat(parts) => {
            for part in parts {
                if let TextSegment::Expr(expr) = part {
                    collect_expr_seed_cells(program, expr, cells);
                }
            }
        }
        IrExpr::FunctionCall { args, .. } | IrExpr::ListConstruct(args) => {
            for arg in args {
                collect_expr_seed_cells(program, arg, cells);
            }
        }
        IrExpr::ObjectConstruct(fields) | IrExpr::TaggedObject { fields, .. } => {
            for (_, value) in fields {
                collect_expr_seed_cells(program, value, cells);
            }
        }
        IrExpr::PatternMatch { source, arms } => {
            cells.insert(*source);
            for (_, body) in arms {
                collect_expr_seed_cells(program, body, cells);
            }
        }
        IrExpr::Constant(_) => {}
    }
}

fn nested_template_cell_ranges(
    program: &IrProgram,
    template_cell_range: (u32, u32),
) -> Vec<(u32, u32)> {
    let mut ranges = Vec::new();
    for node in &program.nodes {
        if let IrNode::ListMap {
            template_cell_range: child_range,
            ..
        } = node
        {
            if *child_range != template_cell_range
                && child_range.0 >= template_cell_range.0
                && child_range.1 <= template_cell_range.1
            {
                ranges.push(*child_range);
            }
        }
    }
    ranges.sort_unstable();
    ranges.dedup();
    ranges
}

fn template_seed_cells(program: &IrProgram, template_cell_range: (u32, u32)) -> Vec<CellId> {
    let mut cells = HashSet::new();
    let nested_ranges = nested_template_cell_ranges(program, template_cell_range);
    let in_nested_range = |cell: CellId| {
        nested_ranges
            .iter()
            .any(|(start, end)| cell.0 >= *start && cell.0 < *end)
    };

    for node in &program.nodes {
        match node {
            IrNode::Element {
                cell,
                kind,
                hovered_cell,
                ..
            } if cell.0 >= template_cell_range.0 && cell.0 < template_cell_range.1 => {
                let mut expr_cells = HashSet::new();
                match kind {
                    ElementKind::Button { label, style }
                    | ElementKind::Label { label, style }
                    | ElementKind::Text { label, style }
                    | ElementKind::Paragraph {
                        content: label,
                        style,
                    }
                    | ElementKind::Link {
                        label, url: style, ..
                    } => {
                        collect_expr_cell_reads(label, &mut expr_cells);
                        collect_expr_cell_reads(style, &mut expr_cells);
                    }
                    ElementKind::TextInput {
                        placeholder,
                        style,
                        text_cell,
                        ..
                    } => {
                        if let Some(placeholder) = placeholder {
                            collect_expr_cell_reads(placeholder, &mut expr_cells);
                        }
                        collect_expr_cell_reads(style, &mut expr_cells);
                        if let Some(text_cell) = text_cell {
                            expr_cells.insert(*text_cell);
                        }
                    }
                    ElementKind::Checkbox {
                        checked,
                        style,
                        icon,
                    } => {
                        if let Some(checked) = checked {
                            expr_cells.insert(*checked);
                        }
                        collect_expr_cell_reads(style, &mut expr_cells);
                        if let Some(icon) = icon {
                            expr_cells.insert(*icon);
                        }
                    }
                    ElementKind::Container { style, .. }
                    | ElementKind::Block { style, .. }
                    | ElementKind::Stack { style, .. }
                    | ElementKind::Svg { style, .. } => {
                        collect_expr_cell_reads(style, &mut expr_cells)
                    }
                    ElementKind::Stripe { gap, style, .. } => {
                        collect_expr_cell_reads(gap, &mut expr_cells);
                        collect_expr_cell_reads(style, &mut expr_cells);
                    }
                    ElementKind::Slider {
                        style, value_cell, ..
                    } => {
                        collect_expr_cell_reads(style, &mut expr_cells);
                        if let Some(value_cell) = value_cell {
                            expr_cells.insert(*value_cell);
                        }
                    }
                    ElementKind::Select {
                        style, selected, ..
                    } => {
                        collect_expr_cell_reads(style, &mut expr_cells);
                        if let Some(selected) = selected {
                            collect_expr_cell_reads(selected, &mut expr_cells);
                        }
                    }
                    ElementKind::SvgCircle { cx, cy, r, style } => {
                        collect_expr_cell_reads(cx, &mut expr_cells);
                        collect_expr_cell_reads(cy, &mut expr_cells);
                        collect_expr_cell_reads(r, &mut expr_cells);
                        collect_expr_cell_reads(style, &mut expr_cells);
                    }
                }
                if let Some(hovered_cell) = hovered_cell {
                    expr_cells.insert(*hovered_cell);
                }
                for expr_cell in expr_cells {
                    if expr_cell.0 >= template_cell_range.0
                        && expr_cell.0 < template_cell_range.1
                        && !in_nested_range(expr_cell)
                    {
                        cells.insert(expr_cell);
                    }
                }
            }
            _ => {}
        }
    }

    let mut cells: Vec<_> = cells.into_iter().collect();
    cells.sort_by_key(|cell| cell.0);
    cells
}

fn template_seed_cells_for_active_root(
    program: &IrProgram,
    cell_store: &CellStore,
    template: Option<&TemplatePlan>,
    bindings: &HashMap<CellId, IrExpr>,
) -> Vec<CellId> {
    let Some(template) = template else {
        return Vec::new();
    };
    let template_root_cell = template.root_cell;
    let template_cell_range = template.cell_range;
    let fallback_cells = || template.seed_cells.clone();

    let Some(root_cell) = template_root_cell else {
        return fallback_cells();
    };

    let Some(active_root_cell) = resolve_active_template_root_cell(
        program,
        cell_store,
        root_cell,
        template_cell_range,
        bindings,
        &mut HashSet::new(),
    ) else {
        return fallback_cells();
    };

    let mut cells = HashSet::new();
    collect_root_selector_seed_cells(program, root_cell, template_cell_range, &mut cells);
    collect_active_element_seed_cells(program, active_root_cell, template_cell_range, &mut cells);

    if cells.is_empty() {
        return fallback_cells();
    }

    let mut cells: Vec<_> = cells.into_iter().collect();
    cells.sort_by_key(|cell| cell.0);
    cells
}

fn template_seed_cells_for_local_events(
    program: &IrProgram,
    template: Option<&TemplatePlan>,
) -> Vec<CellId> {
    let Some(template) = template else {
        return Vec::new();
    };

    let nested_ranges = nested_template_cell_ranges(program, template.cell_range);
    let in_nested_range = |cell: CellId| {
        nested_ranges
            .iter()
            .any(|(start, end)| cell.0 >= *start && cell.0 < *end)
    };
    let in_template_range = |cell: CellId| {
        cell.0 >= template.cell_range.0 && cell.0 < template.cell_range.1 && !in_nested_range(cell)
    };
    let in_template_event_range = |event: EventId| {
        (event.0 >= template.event_range.0 && event.0 < template.event_range.1)
            || template.cross_scope_events.contains(&event.0)
    };

    let mut cells = HashSet::new();

    for node in &program.nodes {
        match node {
            IrNode::Then { trigger, body, .. } if in_template_event_range(*trigger) => {
                collect_expr_seed_cells(program, body, &mut cells);
            }
            IrNode::Hold { trigger_bodies, .. } => {
                for (trigger, body) in trigger_bodies {
                    if in_template_event_range(*trigger) {
                        collect_expr_seed_cells(program, body, &mut cells);
                    }
                }
            }
            IrNode::Latest { arms, .. } => {
                for arm in arms {
                    if arm.trigger.is_some_and(in_template_event_range) {
                        collect_expr_seed_cells(program, &arm.body, &mut cells);
                    }
                }
            }
            _ => {}
        }
    }

    let mut cells: Vec<_> = cells
        .into_iter()
        .filter(|cell| in_template_range(*cell))
        .collect();
    cells.sort_by_key(|cell| cell.0);
    cells.dedup();
    cells
}

fn collect_root_selector_seed_cells(
    program: &IrProgram,
    cell: CellId,
    template_cell_range: (u32, u32),
    cells: &mut HashSet<CellId>,
) {
    let canonical_named_cells = canonical_named_cells(program);
    if cell.0 < template_cell_range.0 || cell.0 >= template_cell_range.1 {
        return;
    }

    match find_node_for_cell(program, cell) {
        Some(IrNode::When { source, .. }) => {
            cells.insert(canonicalize_external_template_cell(
                program,
                &canonical_named_cells,
                *source,
            ));
        }
        Some(IrNode::While { source, deps, .. }) => {
            cells.insert(canonicalize_external_template_cell(
                program,
                &canonical_named_cells,
                *source,
            ));
            for dep in deps {
                cells.insert(canonicalize_external_template_cell(
                    program,
                    &canonical_named_cells,
                    *dep,
                ));
            }
        }
        _ => {}
    }
}

fn resolve_active_template_root_cell(
    program: &IrProgram,
    cell_store: &CellStore,
    cell: CellId,
    template_cell_range: (u32, u32),
    bindings: &HashMap<CellId, IrExpr>,
    seen: &mut HashSet<CellId>,
) -> Option<CellId> {
    if !seen.insert(cell) {
        return None;
    }

    let result = if cell.0 < template_cell_range.0 || cell.0 >= template_cell_range.1 {
        Some(cell)
    } else {
        match find_node_for_cell(program, cell)? {
            IrNode::Element { .. } => Some(cell),
            _ => match resolve_runtime_cell_expr_with_bindings(
                program, cell_store, cell, 0, bindings,
            )? {
                IrExpr::CellRead(next) if next != cell => resolve_active_template_root_cell(
                    program,
                    cell_store,
                    next,
                    template_cell_range,
                    bindings,
                    seen,
                ),
                _ => None,
            },
        }
    };

    seen.remove(&cell);
    result
}

fn collect_active_element_seed_cells(
    program: &IrProgram,
    cell: CellId,
    template_cell_range: (u32, u32),
    cells: &mut HashSet<CellId>,
) {
    if cell.0 < template_cell_range.0 || cell.0 >= template_cell_range.1 {
        cells.insert(cell);
        return;
    }

    let Some(IrNode::Element {
        kind, hovered_cell, ..
    }) = find_node_for_cell(program, cell)
    else {
        cells.insert(cell);
        return;
    };

    let mut expr_cells = HashSet::new();
    match kind {
        ElementKind::Button { label, style }
        | ElementKind::Label { label, style }
        | ElementKind::Text { label, style }
        | ElementKind::Paragraph {
            content: label,
            style,
        }
        | ElementKind::Link {
            label, url: style, ..
        } => {
            collect_expr_cell_reads(label, &mut expr_cells);
            collect_expr_cell_reads(style, &mut expr_cells);
        }
        ElementKind::TextInput {
            placeholder,
            style,
            text_cell,
            ..
        } => {
            if let Some(placeholder) = placeholder {
                collect_expr_cell_reads(placeholder, &mut expr_cells);
            }
            collect_expr_cell_reads(style, &mut expr_cells);
            if let Some(text_cell) = text_cell {
                expr_cells.insert(*text_cell);
            }
        }
        ElementKind::Checkbox {
            checked,
            style,
            icon,
        } => {
            if let Some(checked) = checked {
                expr_cells.insert(*checked);
            }
            collect_expr_cell_reads(style, &mut expr_cells);
            if let Some(icon) = icon {
                expr_cells.insert(*icon);
            }
        }
        ElementKind::Container { style, .. }
        | ElementKind::Block { style, .. }
        | ElementKind::Stack { style, .. }
        | ElementKind::Svg { style, .. } => {
            collect_expr_cell_reads(style, &mut expr_cells);
        }
        ElementKind::Stripe { gap, style, .. } => {
            collect_expr_cell_reads(gap, &mut expr_cells);
            collect_expr_cell_reads(style, &mut expr_cells);
        }
        ElementKind::Slider {
            style, value_cell, ..
        } => {
            collect_expr_cell_reads(style, &mut expr_cells);
            if let Some(value_cell) = value_cell {
                expr_cells.insert(*value_cell);
            }
        }
        ElementKind::Select {
            style, selected, ..
        } => {
            collect_expr_cell_reads(style, &mut expr_cells);
            if let Some(selected) = selected {
                collect_expr_cell_reads(selected, &mut expr_cells);
            }
        }
        ElementKind::SvgCircle { cx, cy, r, style } => {
            collect_expr_cell_reads(cx, &mut expr_cells);
            collect_expr_cell_reads(cy, &mut expr_cells);
            collect_expr_cell_reads(r, &mut expr_cells);
            collect_expr_cell_reads(style, &mut expr_cells);
        }
    }

    if let Some(hovered_cell) = hovered_cell {
        expr_cells.insert(*hovered_cell);
    }

    for expr_cell in expr_cells {
        cells.insert(expr_cell);
    }
}

fn seed_item_template_cells_from_expr(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    ics: &ItemCellStore,
    item_idx: u32,
    template: Option<&TemplatePlan>,
    item_cell: CellId,
    resolved_item_store: CellId,
    item_expr: &IrExpr,
) {
    seed_item_template_cells_from_expr_parts(
        program,
        &instance.cell_store,
        &instance.list_store,
        &instance.template_list_items,
        ics,
        item_idx,
        template,
        item_cell,
        resolved_item_store,
        item_expr,
    )
}

fn seed_item_template_cells_from_expr_parts(
    program: &IrProgram,
    cell_store: &CellStore,
    list_store: &ListStore,
    template_list_items: &Rc<RefCell<HashMap<u64, Vec<IrExpr>>>>,
    ics: &ItemCellStore,
    item_idx: u32,
    template: Option<&TemplatePlan>,
    item_cell: CellId,
    resolved_item_store: CellId,
    item_expr: &IrExpr,
) {
    let bindings = item_bindings(program, item_cell, resolved_item_store, item_expr);
    let seed_resolved_cell =
        |raw_cell: u32, resolved: IrExpr, ics: &ItemCellStore, cell_store: &CellStore| {
            if let Some(list_id) = materialize_template_list_expr(
                program,
                list_store,
                template_list_items,
                cell_store,
                &resolved,
                0,
                &bindings,
            ) {
                ics.set_text(item_idx, raw_cell, String::new());
                ics.set_cell(item_idx, raw_cell, list_id);
                return;
            }

            match resolved {
                IrExpr::Constant(IrValue::Number(n)) => {
                    ics.set_text(item_idx, raw_cell, String::new());
                    ics.set_cell(item_idx, raw_cell, n);
                }
                IrExpr::Constant(IrValue::Bool(b)) => {
                    ics.set_text(item_idx, raw_cell, String::new());
                    ics.set_cell(item_idx, raw_cell, if b { 1.0 } else { 0.0 });
                }
                IrExpr::Constant(IrValue::Text(text)) => {
                    ics.set_text(item_idx, raw_cell, text);
                }
                IrExpr::Constant(IrValue::Tag(tag)) => {
                    ics.set_text(
                        item_idx,
                        raw_cell,
                        visible_tag_text(&tag).unwrap_or_default(),
                    );
                }
                IrExpr::CellRead(global_cell) => {
                    let text = cell_store.get_cell_text(global_cell.0);
                    if !text.is_empty() {
                        ics.set_text(item_idx, raw_cell, text);
                    } else {
                        ics.set_text(item_idx, raw_cell, String::new());
                    }
                    let value = cell_store.get_cell_value(global_cell.0);
                    if !value.is_nan() {
                        ics.set_cell(item_idx, raw_cell, value);
                    }
                }
                _ => {}
            }
        };
    let template_range = template.map(|template| template.cell_range);
    let nested_ranges = template_range
        .map(|range| nested_template_cell_ranges(program, range))
        .unwrap_or_default();

    let in_seedable_template_range = |cell: CellId| {
        template_range.is_some_and(|(start, end)| {
            cell.0 >= start
                && cell.0 < end
                && !nested_ranges.iter().any(|(nested_start, nested_end)| {
                    cell.0 >= *nested_start && cell.0 < *nested_end
                })
        })
    };

    // Pre-seed directly bound item/object field cells so Wasm init_item can reload
    // stable per-item field values like `cell.row` / `cell.column` from the host
    // store before evaluating derived/event bodies.
    for (&cell, expr) in &bindings {
        if !in_seedable_template_range(cell) {
            continue;
        }

        if let Some(list_id) = materialize_template_list_expr(
            program,
            list_store,
            template_list_items,
            cell_store,
            expr,
            0,
            &bindings,
        ) {
            ics.set_text(item_idx, cell.0, String::new());
            ics.set_cell(item_idx, cell.0, list_id);
            continue;
        }

        match resolve_runtime_expr_with_bindings(program, cell_store, expr, 0, &bindings) {
            Some(resolved) => seed_resolved_cell(cell.0, resolved, ics, cell_store),
            None => {}
        }
    }

    // Seed the cells needed by local event-handler bodies, such as
    // `row_data.cells.row` / `row_data.cells.column` in the `cells` edit-start path,
    // without eagerly resolving every template cell up front.
    for cell in template_seed_cells_for_local_events(program, template) {
        if !in_seedable_template_range(cell) {
            continue;
        }
        let Some(resolved) =
            resolve_runtime_cell_expr_with_bindings(program, cell_store, cell, 0, &bindings)
        else {
            continue;
        };
        seed_resolved_cell(cell.0, resolved, ics, cell_store);
    }

    for cell in template_seed_cells_for_active_root(program, cell_store, template, &bindings) {
        let raw_cell = cell.0;
        let Some(resolved) =
            resolve_runtime_cell_expr_with_bindings(program, cell_store, cell, 0, &bindings)
        else {
            continue;
        };
        seed_resolved_cell(raw_cell, resolved, ics, cell_store);
    }
}

fn reseed_item_template_list_cells_from_expr(
    program: &IrProgram,
    instance: &Rc<WasmInstance>,
    ics: &ItemCellStore,
    item_idx: u32,
    template_cell_range: (u32, u32),
    item_cell: CellId,
    item_expr: &IrExpr,
) {
    let cell_store = &instance.cell_store;
    let mut bindings = HashMap::new();
    bindings.insert(item_cell, item_expr.clone());
    let nested_ranges = nested_template_cell_ranges(program, template_cell_range);

    for raw_cell in template_cell_range.0..template_cell_range.1 {
        if nested_ranges
            .iter()
            .any(|(start, end)| raw_cell >= *start && raw_cell < *end)
        {
            continue;
        }
        let cell = CellId(raw_cell);
        let Some(resolved) =
            resolve_runtime_cell_expr_with_bindings(program, cell_store, cell, 0, &bindings)
        else {
            continue;
        };
        let Some(list_id) = materialize_template_list_expr(
            program,
            &instance.list_store,
            &instance.template_list_items,
            cell_store,
            &resolved,
            0,
            &bindings,
        ) else {
            continue;
        };
        ics.set_text(item_idx, raw_cell, String::new());
        ics.set_cell(item_idx, raw_cell, list_id);
    }
}

fn materialize_template_list_expr(
    program: &IrProgram,
    list_store: &ListStore,
    template_list_items: &Rc<RefCell<HashMap<u64, Vec<IrExpr>>>>,
    cell_store: &CellStore,
    expr: &IrExpr,
    depth: u32,
    bindings: &HashMap<CellId, IrExpr>,
) -> Option<f64> {
    if depth > RUNTIME_RESOLVE_DEPTH_LIMIT {
        return None;
    }

    match expr {
        IrExpr::ListConstruct(items) => {
            let list_id = list_store.create();
            list_store.set_index_based(list_id);
            for _ in items {
                list_store.append_with_next_memory_index(list_id);
            }
            template_list_items
                .borrow_mut()
                .insert(list_id.to_bits(), items.clone());
            Some(list_id)
        }
        IrExpr::CellRead(cell) => {
            let value = cell_store.get_cell_value(cell.0);
            if !value.is_nan() && value > 0.0 {
                return Some(value);
            }
            let resolved = resolve_runtime_cell_expr_with_bindings(
                program,
                cell_store,
                *cell,
                depth + 1,
                bindings,
            )?;
            if matches!(resolved, IrExpr::CellRead(next) if next == *cell) {
                None
            } else {
                materialize_template_list_expr(
                    program,
                    list_store,
                    template_list_items,
                    cell_store,
                    &resolved,
                    depth + 1,
                    bindings,
                )
            }
        }
        IrExpr::FieldAccess { object, field } => {
            let fields = resolve_runtime_object_fields_with_bindings(
                program,
                cell_store,
                object,
                depth + 1,
                bindings,
            )?;
            let value = fields
                .into_iter()
                .find(|(name, _)| name == field)
                .map(|(_, value)| value)?;
            materialize_template_list_expr(
                program,
                list_store,
                template_list_items,
                cell_store,
                &value,
                depth + 1,
                bindings,
            )
        }
        _ => None,
    }
}

fn resolve_scene_number(program: &IrProgram, cell_store: &CellStore, expr: &IrExpr) -> Option<f64> {
    match resolve_runtime_expr(program, cell_store, expr, 0).unwrap_or_else(|| expr.clone()) {
        IrExpr::Constant(IrValue::Number(n)) => Some(n),
        IrExpr::CellRead(cell) => {
            let value = cell_store.get_cell_value(cell.0);
            (!value.is_nan()).then_some(value)
        }
        _ => None,
    }
}

fn resolve_scene_object_fields(
    program: &IrProgram,
    cell_store: &CellStore,
    expr: &IrExpr,
) -> Option<Vec<(String, IrExpr)>> {
    match resolve_runtime_expr(program, cell_store, expr, 0).unwrap_or_else(|| expr.clone()) {
        IrExpr::ObjectConstruct(fields) => Some(fields),
        IrExpr::TaggedObject { fields, .. } => Some(fields),
        IrExpr::CellRead(cell) => Some(reconstruct_object_fields(program, cell)),
        _ => None,
    }
}

fn resolve_scene_list_items(
    program: &IrProgram,
    cell_store: &CellStore,
    expr: &IrExpr,
) -> Option<Vec<IrExpr>> {
    match resolve_runtime_expr(program, cell_store, expr, 0).unwrap_or_else(|| expr.clone()) {
        IrExpr::ListConstruct(items) => Some(items),
        IrExpr::CellRead(cell) => match find_node_for_cell(program, cell) {
            Some(IrNode::Derived {
                expr: IrExpr::ListConstruct(items),
                ..
            }) => Some(items.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn resolve_scene_tagged_fields(
    program: &IrProgram,
    cell_store: &CellStore,
    expr: &IrExpr,
) -> Option<(String, Vec<(String, IrExpr)>)> {
    match resolve_runtime_expr(program, cell_store, expr, 0).unwrap_or_else(|| expr.clone()) {
        IrExpr::TaggedObject { tag, fields } => Some((tag, fields)),
        IrExpr::CellRead(cell) => match find_node_for_cell(program, cell) {
            Some(IrNode::Derived {
                expr: IrExpr::TaggedObject { tag, fields },
                ..
            }) => Some((tag.clone(), fields.clone())),
            _ => None,
        },
        _ => None,
    }
}

fn resolve_scene_params(program: &IrProgram, cell_store: &CellStore) -> PhysicalSceneParams {
    let mut params = PhysicalSceneParams::default();
    let Some(render_root) = program.render_root() else {
        return params;
    };
    let Some(scene) = render_root.scene else {
        return params;
    };

    if let Some(geometry) = scene.geometry {
        if let Some(fields) =
            resolve_scene_object_fields(program, cell_store, &IrExpr::CellRead(geometry))
        {
            if let Some((_, bevel_angle)) = fields.iter().find(|(name, _)| name == "bevel_angle") {
                if let Some(bevel_angle) = resolve_scene_number(program, cell_store, bevel_angle) {
                    params.bevel_angle = bevel_angle;
                }
            }
        }
    }

    if let Some(lights) = scene.lights {
        if let Some(items) =
            resolve_scene_list_items(program, cell_store, &IrExpr::CellRead(lights))
        {
            for item in &items {
                let Some((tag, fields)) = resolve_scene_tagged_fields(program, cell_store, item)
                else {
                    continue;
                };
                match tag.as_str() {
                    "DirectionalLight" => {
                        if let Some((_, intensity)) =
                            fields.iter().find(|(name, _)| name == "intensity")
                        {
                            if let Some(intensity) =
                                resolve_scene_number(program, cell_store, intensity)
                            {
                                params.directional_intensity = intensity;
                            }
                        }
                        if let Some((_, spread)) = fields.iter().find(|(name, _)| name == "spread")
                        {
                            if let Some(spread) = resolve_scene_number(program, cell_store, spread)
                            {
                                params.shadow_blur_per_depth = PhysicalSceneParams::DEFAULT
                                    .shadow_blur_per_depth
                                    * spread.clamp(0.25, 4.0);
                            }
                        }
                        let azimuth = fields.iter().find(|(name, _)| name == "azimuth").and_then(
                            |(_, value)| resolve_scene_number(program, cell_store, value),
                        );
                        let altitude = fields.iter().find(|(name, _)| name == "altitude").and_then(
                            |(_, value)| resolve_scene_number(program, cell_store, value),
                        );
                        if let (Some(azimuth), Some(altitude)) = (azimuth, altitude) {
                            let azimuth_radians = azimuth.to_radians();
                            let altitude_radians = altitude.clamp(5.0, 85.0).to_radians();
                            let altitude_factor = (1.0 / altitude_radians.tan()).clamp(0.35, 2.0);
                            params.shadow_dx_per_depth = -azimuth_radians.sin()
                                * PhysicalSceneParams::DEFAULT.shadow_dx_per_depth
                                * altitude_factor;
                            params.shadow_dy_per_depth = azimuth_radians.cos()
                                * PhysicalSceneParams::DEFAULT.shadow_dy_per_depth
                                * altitude_factor;
                        }
                    }
                    "AmbientLight" => {
                        if let Some((_, intensity)) =
                            fields.iter().find(|(name, _)| name == "intensity")
                        {
                            if let Some(intensity) =
                                resolve_scene_number(program, cell_store, intensity)
                            {
                                params.ambient_factor = intensity.clamp(0.0, 1.0);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    params
}

fn collect_scene_param_dependency_cells(program: &IrProgram) -> Vec<CellId> {
    let Some(render_root) = program.render_root() else {
        return Vec::new();
    };
    let Some(scene) = render_root.scene else {
        return Vec::new();
    };

    let mut deps = Vec::new();
    let mut seen = HashSet::new();

    if let Some(lights) = scene.lights {
        collect_scene_expr_dependencies(program, &IrExpr::CellRead(lights), &mut deps, &mut seen);
    }
    if let Some(geometry) = scene.geometry {
        collect_scene_expr_dependencies(program, &IrExpr::CellRead(geometry), &mut deps, &mut seen);
    }

    deps
}

fn collect_scene_expr_dependencies(
    program: &IrProgram,
    expr: &IrExpr,
    deps: &mut Vec<CellId>,
    seen_cells: &mut HashSet<u32>,
) {
    match expr {
        IrExpr::Constant(_) => {}
        IrExpr::CellRead(cell) => {
            if seen_cells.insert(cell.0) {
                deps.push(*cell);
                if let Some(node) = find_node_for_cell(program, *cell) {
                    collect_scene_node_dependencies(program, node, deps, seen_cells);
                }
            }
        }
        IrExpr::FieldAccess { object, .. } => {
            collect_scene_expr_dependencies(program, object, deps, seen_cells);
        }
        IrExpr::BinOp { lhs, rhs, .. } | IrExpr::Compare { lhs, rhs, .. } => {
            collect_scene_expr_dependencies(program, lhs, deps, seen_cells);
            collect_scene_expr_dependencies(program, rhs, deps, seen_cells);
        }
        IrExpr::UnaryNeg(inner) | IrExpr::Not(inner) => {
            collect_scene_expr_dependencies(program, inner, deps, seen_cells);
        }
        IrExpr::TextConcat(parts) => {
            for part in parts {
                if let TextSegment::Expr(expr) = part {
                    collect_scene_expr_dependencies(program, expr, deps, seen_cells);
                }
            }
        }
        IrExpr::FunctionCall { args, .. } => {
            for arg in args {
                collect_scene_expr_dependencies(program, arg, deps, seen_cells);
            }
        }
        IrExpr::ObjectConstruct(fields) | IrExpr::TaggedObject { fields, .. } => {
            for (_, value) in fields {
                collect_scene_expr_dependencies(program, value, deps, seen_cells);
            }
        }
        IrExpr::ListConstruct(items) => {
            for item in items {
                collect_scene_expr_dependencies(program, item, deps, seen_cells);
            }
        }
        IrExpr::PatternMatch { source, arms } => {
            collect_scene_expr_dependencies(program, &IrExpr::CellRead(*source), deps, seen_cells);
            for (_, body) in arms {
                collect_scene_expr_dependencies(program, body, deps, seen_cells);
            }
        }
    }
}

fn collect_scene_node_dependencies(
    program: &IrProgram,
    node: &IrNode,
    deps: &mut Vec<CellId>,
    seen_cells: &mut HashSet<u32>,
) {
    match node {
        IrNode::Derived { expr, .. } => {
            collect_scene_expr_dependencies(program, expr, deps, seen_cells);
        }
        IrNode::PipeThrough { source, .. } => {
            collect_scene_expr_dependencies(program, &IrExpr::CellRead(*source), deps, seen_cells);
        }
        IrNode::When { source, arms, .. } | IrNode::While { source, arms, .. } => {
            collect_scene_expr_dependencies(program, &IrExpr::CellRead(*source), deps, seen_cells);
            for (_, body) in arms {
                collect_scene_expr_dependencies(program, body, deps, seen_cells);
            }
        }
        IrNode::TextInterpolation { parts, .. } => {
            for part in parts {
                if let TextSegment::Expr(expr) = part {
                    collect_scene_expr_dependencies(program, expr, deps, seen_cells);
                }
            }
        }
        IrNode::Document {
            lights, geometry, ..
        } => {
            if let Some(lights) = lights {
                collect_scene_expr_dependencies(
                    program,
                    &IrExpr::CellRead(*lights),
                    deps,
                    seen_cells,
                );
            }
            if let Some(geometry) = geometry {
                collect_scene_expr_dependencies(
                    program,
                    &IrExpr::CellRead(*geometry),
                    deps,
                    seen_cells,
                );
            }
        }
        _ => {}
    }
}

fn initialize_scene_params(instance: &Rc<WasmInstance>) {
    refresh_scene_params(
        &instance.program,
        &instance.cell_store,
        &instance.scene_params,
    );

    let deps = collect_scene_param_dependency_cells(&instance.program);
    for cell in deps {
        let inst = instance.clone();
        let handle = Task::start_droppable(inst.cell_store.get_cell_signal(cell.0).for_each_sync(
            move |_| {
                refresh_scene_params(&inst.program, &inst.cell_store, &inst.scene_params);
            },
        ));
        std::mem::forget(handle);
    }
}

fn refresh_scene_params(
    program: &IrProgram,
    cell_store: &CellStore,
    scene_params: &Mutable<PhysicalSceneParams>,
) -> PhysicalSceneParams {
    let scene = resolve_scene_params(program, cell_store);
    scene_params.set_neq(scene);
    scene
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
            let has_oklch = fields
                .iter()
                .any(|(n, _)| n == "lightness" || n == "chroma" || n == "hue");
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
                                    if v.is_nan() {
                                        global_store.get_cell_value(c)
                                    } else {
                                        v
                                    }
                                });
                                let c = chroma_cell.map_or(chroma_default, |c2| {
                                    let v = item_store.get_value(item_idx, c2);
                                    if v.is_nan() {
                                        global_store.get_cell_value(c2)
                                    } else {
                                        v
                                    }
                                });
                                let h = hue_cell.map_or(hue_default, |c3| {
                                    let v = item_store.get_value(item_idx, c3);
                                    if v.is_nan() {
                                        global_store.get_cell_value(c3)
                                    } else {
                                        v
                                    }
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
                                if v.is_nan() {
                                    global_store.get_cell_value(c)
                                } else {
                                    v
                                }
                            });
                            let c = chroma_cell.map_or(chroma_default, |c2| {
                                let v = item_store.get_value(item_idx, c2);
                                if v.is_nan() {
                                    global_store.get_cell_value(c2)
                                } else {
                                    v
                                }
                            });
                            let h = hue_cell.map_or(hue_default, |c3| {
                                let v = item_store.get_value(item_idx, c3);
                                if v.is_nan() {
                                    global_store.get_cell_value(c3)
                                } else {
                                    v
                                }
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

fn node_output_cell(node: &IrNode) -> Option<CellId> {
    match node {
        IrNode::Derived { cell, .. }
        | IrNode::Hold { cell, .. }
        | IrNode::Then { cell, .. }
        | IrNode::When { cell, .. }
        | IrNode::While { cell, .. }
        | IrNode::Element { cell, .. }
        | IrNode::TextInterpolation { cell, .. }
        | IrNode::MathSum { cell, .. }
        | IrNode::PipeThrough { cell, .. }
        | IrNode::StreamSkip { cell, .. }
        | IrNode::CustomCall { cell, .. }
        | IrNode::ListAppend { cell, .. }
        | IrNode::ListClear { cell, .. }
        | IrNode::ListCount { cell, .. }
        | IrNode::ListMap { cell, .. }
        | IrNode::ListRemove { cell, .. }
        | IrNode::ListRetain { cell, .. }
        | IrNode::ListEvery { cell, .. }
        | IrNode::ListAny { cell, .. }
        | IrNode::ListIsEmpty { cell, .. }
        | IrNode::RouterGoTo { cell, .. }
        | IrNode::TextTrim { cell, .. }
        | IrNode::TextIsNotEmpty { cell, .. }
        | IrNode::TextToNumber { cell, .. }
        | IrNode::TextStartsWith { cell, .. }
        | IrNode::MathRound { cell, .. }
        | IrNode::MathMin { cell, .. }
        | IrNode::MathMax { cell, .. }
        | IrNode::HoldLoop { cell, .. } => Some(*cell),
        IrNode::Latest { target, .. } => Some(*target),
        IrNode::Document { root, .. } => Some(*root),
        IrNode::Timer { .. } => None,
    }
}

fn resolve_inline_template_expr_with_bindings(
    program: &IrProgram,
    cell_store: &CellStore,
    node: &IrNode,
    depth: u32,
    bindings: &HashMap<CellId, IrExpr>,
) -> Option<IrExpr> {
    match node {
        IrNode::Derived { expr, .. } => {
            resolve_runtime_expr_with_bindings(program, cell_store, expr, depth + 1, bindings)
                .or_else(|| Some(expr.clone()))
        }
        IrNode::PipeThrough { source, .. } => resolve_runtime_cell_expr_with_bindings(
            program,
            cell_store,
            *source,
            depth + 1,
            bindings,
        )
        .or_else(|| Some(IrExpr::CellRead(*source))),
        IrNode::Hold { init, .. } => {
            resolve_runtime_expr_with_bindings(program, cell_store, init, depth + 1, bindings)
                .or_else(|| Some(init.clone()))
        }
        _ => node_output_cell(node).map(IrExpr::CellRead),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::{HashMap, HashSet};
    use std::rc::Rc;

    use super::{
        CellStore, ItemContext, collect_cross_scope_events, collect_scene_param_dependency_cells,
        collect_template_global_dependencies, compute_physical_depth_box_shadow,
        compute_physical_gloss_background, find_data_cells_for_event, find_node_for_cell,
        item_bindings, materialize_template_list_expr, reconstruct_object_fields,
        refresh_scene_params, resolve_active_template_root_cell, resolve_parent_template_list_id,
        resolve_runtime_cell_expr_with_bindings, resolve_runtime_list_item_expr,
        resolve_scene_params, seed_item_template_cells_from_expr_parts,
        template_seed_cells_for_active_root, template_seed_cells_for_local_events,
    };
    use crate::platform::browser::engine_wasm::{
        ir::{CellId, ElementKind, IrExpr, IrNode, IrProgram, IrValue},
        lower::lower,
        parse_source,
        runtime::{ItemCellStore, ListStore},
    };
    use boon_scene::{PhysicalSceneParams, RenderSurface};
    use zoon::Mutable;

    fn list_map_bindings(
        program: &IrProgram,
        map_cell: CellId,
        item_cell: CellId,
        item_expr: &IrExpr,
    ) -> HashMap<CellId, IrExpr> {
        let resolved_item_store = program
            .list_map_plan(map_cell)
            .map(|plan| plan.resolved_item_store)
            .unwrap_or(item_cell);
        item_bindings(program, item_cell, resolved_item_store, item_expr)
    }

    #[test]
    fn resolve_scene_params_uses_compiled_light_and_geometry_cells() {
        let program = IrProgram {
            cells: Vec::new(),
            events: Vec::new(),
            nodes: vec![
                IrNode::Document {
                    kind: RenderSurface::Scene,
                    root: CellId(1),
                    lights: Some(CellId(2)),
                    geometry: Some(CellId(3)),
                },
                IrNode::Derived {
                    cell: CellId(2),
                    expr: IrExpr::ListConstruct(vec![
                        IrExpr::TaggedObject {
                            tag: "DirectionalLight".to_string(),
                            fields: vec![
                                ("azimuth".to_string(), IrExpr::CellRead(CellId(10))),
                                ("altitude".to_string(), IrExpr::CellRead(CellId(11))),
                                ("spread".to_string(), IrExpr::CellRead(CellId(12))),
                                ("intensity".to_string(), IrExpr::CellRead(CellId(13))),
                            ],
                        },
                        IrExpr::TaggedObject {
                            tag: "AmbientLight".to_string(),
                            fields: vec![("intensity".to_string(), IrExpr::CellRead(CellId(14)))],
                        },
                    ]),
                },
                IrNode::Derived {
                    cell: CellId(3),
                    expr: IrExpr::ObjectConstruct(vec![(
                        "bevel_angle".to_string(),
                        IrExpr::CellRead(CellId(15)),
                    )]),
                },
            ],
            document: Some(CellId(1)),
            render_surface: Some(RenderSurface::Scene),
            functions: Vec::new(),
            tag_table: Vec::new(),
            cell_field_cells: HashMap::new(),
            list_map_plans: HashMap::new(),
        };

        let store = CellStore::new(16);
        store.set_cell_f64(10, 30.0);
        store.set_cell_f64(11, 45.0);
        store.set_cell_f64(12, 1.5);
        store.set_cell_f64(13, 1.2);
        store.set_cell_f64(14, 0.4);
        store.set_cell_f64(15, 50.0);

        let scene = resolve_scene_params(&program, &store);

        assert_eq!(scene.directional_intensity, 1.2);
        assert_eq!(scene.ambient_factor, 0.4);
        assert_eq!(scene.bevel_angle, 50.0);
        assert_eq!(
            scene.shadow_blur_per_depth,
            boon_scene::PhysicalSceneParams::DEFAULT.shadow_blur_per_depth * 1.5
        );
        assert_ne!(
            scene.shadow_dx_per_depth,
            boon_scene::PhysicalSceneParams::DEFAULT.shadow_dx_per_depth
        );
        assert_ne!(
            scene.shadow_dy_per_depth,
            boon_scene::PhysicalSceneParams::DEFAULT.shadow_dy_per_depth
        );
    }

    #[test]
    fn physical_css_helpers_reflect_scene_params() {
        let scene = PhysicalSceneParams {
            shadow_dx_per_depth: 2.0,
            shadow_dy_per_depth: 3.0,
            shadow_blur_per_depth: 4.0,
            directional_intensity: 1.2,
            ambient_factor: 0.4,
            bevel_angle: 50.0,
        };

        let shadow = compute_physical_depth_box_shadow(2.0, scene).expect("shadow");
        assert!(shadow.contains("4.0px 6.0px 8.0px"));
        assert!(shadow.contains("rgba(0,0,0,0.22)"));

        let gloss = compute_physical_gloss_background(0.5, scene).expect("gloss");
        assert!(gloss.contains("linear-gradient(50deg"));
        assert!(gloss.contains("rgba(255,255,255,0.12)"));
    }

    #[test]
    fn collect_scene_param_dependency_cells_finds_nested_light_and_geometry_cells() {
        let program = IrProgram {
            cells: Vec::new(),
            events: Vec::new(),
            nodes: vec![
                IrNode::Document {
                    kind: RenderSurface::Scene,
                    root: CellId(1),
                    lights: Some(CellId(2)),
                    geometry: Some(CellId(3)),
                },
                IrNode::Derived {
                    cell: CellId(2),
                    expr: IrExpr::ListConstruct(vec![IrExpr::TaggedObject {
                        tag: "DirectionalLight".to_string(),
                        fields: vec![
                            ("azimuth".to_string(), IrExpr::CellRead(CellId(10))),
                            ("altitude".to_string(), IrExpr::CellRead(CellId(11))),
                            ("spread".to_string(), IrExpr::CellRead(CellId(12))),
                            ("intensity".to_string(), IrExpr::CellRead(CellId(13))),
                        ],
                    }]),
                },
                IrNode::Derived {
                    cell: CellId(3),
                    expr: IrExpr::ObjectConstruct(vec![(
                        "bevel_angle".to_string(),
                        IrExpr::CellRead(CellId(15)),
                    )]),
                },
            ],
            document: Some(CellId(1)),
            render_surface: Some(RenderSurface::Scene),
            functions: Vec::new(),
            tag_table: Vec::new(),
            cell_field_cells: HashMap::new(),
            list_map_plans: HashMap::new(),
        };

        let deps = collect_scene_param_dependency_cells(&program);
        let dep_ids: Vec<u32> = deps.into_iter().map(|cell| cell.0).collect();

        assert!(dep_ids.contains(&2));
        assert!(dep_ids.contains(&3));
        assert!(dep_ids.contains(&10));
        assert!(dep_ids.contains(&11));
        assert!(dep_ids.contains(&12));
        assert!(dep_ids.contains(&13));
        assert!(dep_ids.contains(&15));
    }

    #[test]
    fn refresh_scene_params_updates_shared_state_after_cell_changes() {
        let program = IrProgram {
            cells: Vec::new(),
            events: Vec::new(),
            nodes: vec![
                IrNode::Document {
                    kind: RenderSurface::Scene,
                    root: CellId(1),
                    lights: Some(CellId(2)),
                    geometry: Some(CellId(3)),
                },
                IrNode::Derived {
                    cell: CellId(2),
                    expr: IrExpr::ListConstruct(vec![IrExpr::TaggedObject {
                        tag: "DirectionalLight".to_string(),
                        fields: vec![
                            ("azimuth".to_string(), IrExpr::CellRead(CellId(10))),
                            ("altitude".to_string(), IrExpr::CellRead(CellId(11))),
                            ("spread".to_string(), IrExpr::CellRead(CellId(12))),
                            ("intensity".to_string(), IrExpr::CellRead(CellId(13))),
                        ],
                    }]),
                },
                IrNode::Derived {
                    cell: CellId(3),
                    expr: IrExpr::ObjectConstruct(vec![(
                        "bevel_angle".to_string(),
                        IrExpr::CellRead(CellId(15)),
                    )]),
                },
            ],
            document: Some(CellId(1)),
            render_surface: Some(RenderSurface::Scene),
            functions: Vec::new(),
            tag_table: Vec::new(),
            cell_field_cells: HashMap::new(),
            list_map_plans: HashMap::new(),
        };

        let store = CellStore::new(16);
        store.set_cell_f64(10, 25.0);
        store.set_cell_f64(11, 40.0);
        store.set_cell_f64(12, 1.0);
        store.set_cell_f64(13, 0.8);
        store.set_cell_f64(15, 35.0);

        let scene_params = Mutable::new(PhysicalSceneParams::default());
        let initial = refresh_scene_params(&program, &store, &scene_params);

        assert_eq!(initial.directional_intensity, 0.8);
        assert_eq!(initial.bevel_angle, 35.0);
        assert_eq!(scene_params.get().directional_intensity, 0.8);

        store.set_cell_f64(12, 1.8);
        store.set_cell_f64(13, 1.4);
        store.set_cell_f64(15, 70.0);

        let updated = refresh_scene_params(&program, &store, &scene_params);

        assert_eq!(updated.directional_intensity, 1.4);
        assert_eq!(updated.bevel_angle, 70.0);
        assert_eq!(scene_params.get().directional_intensity, 1.4);
        assert_eq!(scene_params.get().bevel_angle, 70.0);
        assert!(
            updated.shadow_blur_per_depth > initial.shadow_blur_per_depth,
            "light spread should increase blur"
        );
    }

    #[test]
    fn slider_change_value_cells_use_timer_link_payloads() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/timer/timer.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");

        let slider_links = program.nodes.iter().find_map(|node| match node {
            IrNode::Element { kind, links, .. } if matches!(kind, ElementKind::Slider { .. }) => {
                Some(links.clone())
            }
            _ => None,
        });
        let Some(links) = slider_links else {
            panic!("timer slider not found");
        };

        let value_cells = find_data_cells_for_event(&program, &links, "change", "value");
        let value_cell_names: Vec<String> = value_cells
            .iter()
            .map(|cell_id| program.cells[*cell_id as usize].name.clone())
            .collect();

        assert!(
            value_cell_names
                .iter()
                .any(|name| name == "store.elements.duration_slider.event.change.value"),
            "expected linked slider payload cell, got {:?}",
            value_cell_names
        );
    }

    #[test]
    fn cells_row_template_resolves_first_row_number_from_item_binding() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/cells/cells.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");
        let cell_store = CellStore::new(program.cells.len());

        let (map_cell, source_cell, item_cell, template_cell_range) = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::ListMap {
                    cell,
                    source,
                    item_name,
                    item_cell,
                    template_cell_range,
                    ..
                } if item_name == "row_data" => {
                    Some((*cell, *source, *item_cell, *template_cell_range))
                }
                _ => None,
            })
            .expect("outer row list map");
        println!(
            "outer row map node: {:?}",
            super::find_node_for_cell(&program, map_cell)
        );

        let item_expr =
            resolve_runtime_list_item_expr(&program, &cell_store, map_cell, 0).expect("item expr");
        let bindings = list_map_bindings(&program, map_cell, item_cell, &item_expr);
        let row_like_cells: Vec<(u32, String)> = program
            .cells
            .iter()
            .enumerate()
            .filter_map(|(idx, cell)| {
                ((idx as u32) >= template_cell_range.0
                    && (idx as u32) < template_cell_range.1
                    && cell.name.contains("row"))
                .then_some((idx as u32, cell.name.clone()))
            })
            .collect();
        let preferred_row_names = ["row_data.row", "object.row", "row"];
        let row_cell = preferred_row_names
            .iter()
            .find_map(|target| {
                row_like_cells
                    .iter()
                    .find_map(|(idx, name)| (name == target).then_some(CellId(*idx)))
            })
            .or_else(|| {
                row_like_cells.iter().find_map(|(idx, name)| {
                    (name.ends_with(".row") && !name.contains("cells.")).then_some(CellId(*idx))
                })
            })
            .unwrap_or_else(|| panic!("row-like cells: {:?}", row_like_cells));
        println!("row_like_cells={row_like_cells:?}");
        println!(
            "row_cell_node={:?}",
            super::find_node_for_cell(&program, row_cell)
        );

        let resolved =
            resolve_runtime_cell_expr_with_bindings(&program, &cell_store, row_cell, 0, &bindings)
                .expect("resolved row expr");

        assert!(
            matches!(resolved, IrExpr::Constant(IrValue::Number(n)) if (n - 1.0).abs() < f64::EPSILON),
            "expected row 1, got {resolved:?}; item_expr={:?}; reconstructed={:?}",
            bindings.get(&item_cell),
            bindings.get(&item_cell).and_then(|expr| match expr {
                IrExpr::CellRead(cell) => Some(reconstruct_object_fields(&program, *cell)),
                IrExpr::ObjectConstruct(fields) => Some(fields.clone()),
                _ => None,
            })
        );
    }

    #[test]
    fn cells_row_template_resolves_first_row_cells_from_item_binding() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/cells/cells.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");
        let cell_store = CellStore::new(program.cells.len());

        let (map_cell, source_cell, item_cell, template_cell_range) = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::ListMap {
                    cell,
                    source,
                    item_name,
                    item_cell,
                    template_cell_range,
                    ..
                } if item_name == "row_data" => {
                    Some((*cell, *source, *item_cell, *template_cell_range))
                }
                _ => None,
            })
            .expect("outer row list map");

        let item_expr =
            resolve_runtime_list_item_expr(&program, &cell_store, map_cell, 0).expect("item expr");
        let bindings = list_map_bindings(&program, map_cell, item_cell, &item_expr);
        let row_cells_cell = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::ListMap {
                    cell,
                    source,
                    item_name,
                    ..
                } if cell.0 >= template_cell_range.0
                    && cell.0 < template_cell_range.1
                    && item_name == "cell" =>
                {
                    Some(*source)
                }
                _ => None,
            })
            .expect("inner row cell list source");

        let resolved = resolve_runtime_cell_expr_with_bindings(
            &program,
            &cell_store,
            row_cells_cell,
            0,
            &bindings,
        )
        .expect("resolved row_cells expr");

        println!(
            "row_cells source node: {:?}",
            super::find_node_for_cell(&program, row_cells_cell)
        );
        println!("resolved row_cells expr: {resolved:?}");
        assert!(
            matches!(
                resolved,
                IrExpr::FieldAccess { .. } | IrExpr::ListConstruct(_) | IrExpr::CellRead(_)
            ),
            "expected row cells path to stay resolvable, got {resolved:?}"
        );
    }

    #[test]
    fn cells_row_template_materializes_first_row_cells_list_from_item_binding() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/cells/cells.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");
        let cell_store = CellStore::new(program.cells.len());
        let list_store = ListStore::new();
        let template_list_items = Rc::new(RefCell::new(HashMap::new()));

        let (map_cell, source_cell, item_cell, template_cell_range) = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::ListMap {
                    cell,
                    source,
                    item_name,
                    item_cell,
                    template_cell_range,
                    ..
                } if item_name == "row_data" => {
                    Some((*cell, *source, *item_cell, *template_cell_range))
                }
                _ => None,
            })
            .expect("outer row list map");

        let item_expr =
            resolve_runtime_list_item_expr(&program, &cell_store, map_cell, 0).expect("item expr");
        let bindings = list_map_bindings(&program, map_cell, item_cell, &item_expr);
        let row_cells_cell = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::ListMap {
                    cell,
                    source,
                    item_name,
                    ..
                } if cell.0 >= template_cell_range.0
                    && cell.0 < template_cell_range.1
                    && item_name == "cell" =>
                {
                    Some(*source)
                }
                _ => None,
            })
            .expect("inner row cell list source");

        let resolved = resolve_runtime_cell_expr_with_bindings(
            &program,
            &cell_store,
            row_cells_cell,
            0,
            &bindings,
        )
        .expect("resolved row_cells expr");
        let list_id = materialize_template_list_expr(
            &program,
            &list_store,
            &template_list_items,
            &cell_store,
            &resolved,
            0,
            &bindings,
        )
        .expect("materialized row cell list");

        assert!(list_id > 0.0, "expected concrete list id, got {list_id}");
        let items = template_list_items
            .borrow()
            .get(&list_id.to_bits())
            .cloned()
            .expect("template list items");
        assert_eq!(items.len(), 26, "expected 26 cells in first row");
    }

    #[test]
    fn cells_nested_map_recovers_first_row_cells_list_when_map_cell_is_unseeded() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/cells/cells.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");
        let cell_store = CellStore::new(program.cells.len());
        let list_store = ListStore::new();
        let template_list_items = Rc::new(RefCell::new(HashMap::new()));
        let template_ranges: Vec<(u32, u32)> = program
            .nodes
            .iter()
            .filter_map(|node| match node {
                IrNode::ListMap {
                    template_cell_range,
                    ..
                } => Some(*template_cell_range),
                _ => None,
            })
            .collect();
        let item_store = ItemCellStore::new(template_ranges);

        let (outer_map_cell, outer_item_cell, outer_template_cell_range) = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::ListMap {
                    cell,
                    item_name,
                    item_cell,
                    template_cell_range,
                    ..
                } if item_name == "row_data" => Some((*cell, *item_cell, *template_cell_range)),
                _ => None,
            })
            .expect("outer row list map");
        let (inner_map_cell, inner_source_cell) = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::ListMap {
                    cell,
                    source,
                    item_name,
                    template_cell_range,
                    ..
                } if item_name == "cell"
                    && cell.0 >= outer_template_cell_range.0
                    && cell.0 < outer_template_cell_range.1
                    && source.0 >= outer_template_cell_range.0
                    && source.0 < outer_template_cell_range.1 =>
                {
                    Some((*cell, *source))
                }
                _ => None,
            })
            .expect("nested cell list map");

        let outer_item_expr =
            resolve_runtime_list_item_expr(&program, &cell_store, outer_map_cell, 0)
                .expect("first row item expr");
        let outer_plan = program
            .list_map_plan(outer_map_cell)
            .expect("outer row list map plan");
        let parent_ctx = ItemContext {
            item_idx: 0,
            propagation_item_idx: 0,
            item_cell_id: outer_item_cell.0,
            resolved_item_store_id: outer_plan.resolved_item_store.0,
            template_cell_range: outer_plan.template.cell_range,
            template_event_range: outer_plan.template.event_range,
            item_expr: Some(outer_item_expr),
        };

        item_store.ensure_item(parent_ctx.item_idx);
        assert!(
            item_store
                .get_value(parent_ctx.item_idx, inner_map_cell.0)
                .is_nan(),
            "expected nested map cell to start unseeded"
        );

        let list_id = resolve_parent_template_list_id(
            &program,
            &cell_store,
            &list_store,
            &template_list_items,
            Some(&item_store),
            &parent_ctx,
            inner_map_cell,
            Some(inner_source_cell),
        )
        .expect("nested row cell list id");

        assert!(list_id > 0.0, "expected concrete nested list id");
        assert_eq!(
            item_store.get_value(parent_ctx.item_idx, inner_map_cell.0),
            list_id,
            "expected fallback to cache nested list id onto the parent map cell"
        );
        let items: Vec<IrExpr> = template_list_items
            .borrow()
            .get(&list_id.to_bits())
            .cloned()
            .expect("nested template items");
        assert_eq!(
            items.len(),
            26,
            "expected first row to expose 26 cell items"
        );
    }

    #[test]
    fn cells_first_cell_template_resolves_seed_formula_and_value() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/cells/cells.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");
        let cell_store = CellStore::new(program.cells.len());
        let list_store = ListStore::new();
        let template_list_items = Rc::new(RefCell::new(HashMap::new()));

        let all_cell_maps: Vec<(CellId, CellId, CellId, (u32, u32), String)> = program
            .nodes
            .iter()
            .filter_map(|node| match node {
                IrNode::ListMap {
                    cell,
                    source,
                    item_name,
                    item_cell,
                    template,
                    template_cell_range,
                    ..
                } if item_name == "cell" => Some((
                    *cell,
                    *source,
                    *item_cell,
                    *template_cell_range,
                    format!("{template:?}"),
                )),
                _ => None,
            })
            .collect();
        let (inner_map_cell, inner_source_cell, inner_item_cell, inner_template_cell_range, _) =
            all_cell_maps
                .first()
                .cloned()
                .expect("inner row cell list map");

        let all_row_maps: Vec<(CellId, CellId, CellId, (u32, u32), String)> = program
            .nodes
            .iter()
            .filter_map(|node| match node {
                IrNode::ListMap {
                    cell,
                    source,
                    item_name,
                    item_cell,
                    template,
                    template_cell_range,
                    ..
                } if item_name == "row_data" => Some((
                    *cell,
                    *source,
                    *item_cell,
                    *template_cell_range,
                    format!("{template:?}"),
                )),
                _ => None,
            })
            .collect();
        let (outer_map_cell, outer_source_cell, outer_item_cell, outer_template_cell_range, _) =
            all_row_maps
                .iter()
                .find(|(_, _, _, range, _)| {
                    inner_map_cell.0 >= range.0 && inner_map_cell.0 < range.1
                })
                .cloned()
                .expect("outer render row list map");

        let outer_item_expr =
            resolve_runtime_list_item_expr(&program, &cell_store, outer_map_cell, 0)
                .expect("first row item expr");
        let outer_bindings =
            list_map_bindings(&program, outer_map_cell, outer_item_cell, &outer_item_expr);

        let inner_source_expr = resolve_runtime_cell_expr_with_bindings(
            &program,
            &cell_store,
            inner_source_cell,
            0,
            &outer_bindings,
        )
        .expect("resolved row cells expr");
        let inner_list_id = materialize_template_list_expr(
            &program,
            &list_store,
            &template_list_items,
            &cell_store,
            &inner_source_expr,
            0,
            &outer_bindings,
        )
        .expect("materialized first row cells");
        let first_cell_expr = template_list_items
            .borrow()
            .get(&inner_list_id.to_bits())
            .and_then(|items| items.first().cloned())
            .expect("first cell item expr");

        let inner_bindings =
            list_map_bindings(&program, inner_map_cell, inner_item_cell, &first_cell_expr);
        let interesting_cells: Vec<(u32, String)> = program
            .cells
            .iter()
            .enumerate()
            .filter_map(|(idx, cell)| {
                let raw = idx as u32;
                ((raw >= inner_template_cell_range.0 && raw < inner_template_cell_range.1)
                    && (cell.name.contains("formula_text")
                        || cell.name.contains("display_value")
                        || cell.name.contains("is_editing")
                        || cell.name.contains("input_text")
                        || cell.name.contains("Text/is_empty")
                        || cell.name.contains("cell.column")
                        || cell.name.contains("cell.row")))
                .then_some((raw, cell.name.clone()))
            })
            .collect();

        let formula_text_cell = interesting_cells
            .iter()
            .find_map(|(raw, name)| {
                (name.ends_with(".formula_text")
                    || name.contains("formula_text")
                    || name.contains("cell_formula"))
                .then_some(CellId(*raw))
            })
            .unwrap_or_else(|| {
                panic!("formula_text-like cell missing; interesting={interesting_cells:?}")
            });
        let formula_text = resolve_runtime_cell_expr_with_bindings(
            &program,
            &cell_store,
            formula_text_cell,
            0,
            &inner_bindings,
        )
        .expect("resolved formula_text");
        assert!(
            matches!(formula_text, IrExpr::Constant(IrValue::Text(ref t)) if t == "5"),
            "expected first cell formula text to resolve to '5', got {formula_text:?}"
        );

        let display_value_cell = interesting_cells
            .iter()
            .find_map(|(raw, name)| {
                (name.ends_with(".display_value") || name.contains("display_value"))
                    .then_some(CellId(*raw))
            })
            .unwrap_or_else(|| {
                panic!("display_value-like cell missing; interesting={interesting_cells:?}")
            });
        let display_value = resolve_runtime_cell_expr_with_bindings(
            &program,
            &cell_store,
            display_value_cell,
            0,
            &inner_bindings,
        )
        .expect("resolved display_value");
        assert!(
            matches!(display_value, IrExpr::Constant(IrValue::Number(n)) if (n - 5.0).abs() < f64::EPSILON),
            "expected first cell display value to resolve to 5, got {display_value:?}"
        );

        let display_label_cell = program
            .nodes
            .iter()
            .filter_map(|node| match node {
                IrNode::Element {
                    cell,
                    kind: ElementKind::Label { label, .. },
                    links,
                    ..
                } if cell.0 >= inner_template_cell_range.0
                    && cell.0 < inner_template_cell_range.1
                    && links.iter().any(|(name, _)| name == "double_click") =>
                {
                    match label {
                        IrExpr::CellRead(label_cell) => Some(*label_cell),
                        _ => None,
                    }
                }
                _ => None,
            })
            .next()
            .expect("display label cell");
        let display_label = resolve_runtime_cell_expr_with_bindings(
            &program,
            &cell_store,
            display_label_cell,
            0,
            &inner_bindings,
        )
        .expect("resolved display label");
        assert!(
            matches!(display_label, IrExpr::Constant(IrValue::Text(ref t)) if t == "5"),
            "expected first cell label text to resolve to '5', got {display_label:?}"
        );
    }

    #[test]
    fn cells_second_and_third_cell_templates_resolve_formula_values() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/cells/cells.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");
        let cell_store = CellStore::new(program.cells.len());
        let list_store = ListStore::new();
        let template_list_items = Rc::new(RefCell::new(HashMap::new()));

        let (inner_map_cell, inner_source_cell, inner_item_cell, inner_template_cell_range, _) =
            program
                .nodes
                .iter()
                .filter_map(|node| match node {
                    IrNode::ListMap {
                        cell,
                        source,
                        item_name,
                        item_cell,
                        template,
                        template_cell_range,
                        ..
                    } if item_name == "cell" => Some((
                        *cell,
                        *source,
                        *item_cell,
                        *template_cell_range,
                        format!("{template:?}"),
                    )),
                    _ => None,
                })
                .find(|(_, source, _, _, _)| *source == CellId(42417))
                .expect("inner row cell list map");

        let (outer_map_cell, outer_source_cell, outer_item_cell) = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::ListMap {
                    cell,
                    source,
                    item_name,
                    item_cell,
                    template_cell_range,
                    ..
                } if item_name == "row_data"
                    && inner_map_cell.0 >= template_cell_range.0
                    && inner_map_cell.0 < template_cell_range.1 =>
                {
                    Some((*cell, *source, *item_cell))
                }
                _ => None,
            })
            .expect("outer render row list map");

        let outer_item_expr =
            resolve_runtime_list_item_expr(&program, &cell_store, outer_map_cell, 0)
                .expect("first row item expr");
        let outer_bindings =
            list_map_bindings(&program, outer_map_cell, outer_item_cell, &outer_item_expr);
        let inner_source_expr = resolve_runtime_cell_expr_with_bindings(
            &program,
            &cell_store,
            inner_source_cell,
            0,
            &outer_bindings,
        )
        .expect("resolved row cells expr");
        let inner_list_id = materialize_template_list_expr(
            &program,
            &list_store,
            &template_list_items,
            &cell_store,
            &inner_source_expr,
            0,
            &outer_bindings,
        )
        .expect("materialized first row cells");
        let inner_items = template_list_items
            .borrow()
            .get(&inner_list_id.to_bits())
            .cloned()
            .expect("inner items");

        let interesting_cells: Vec<(u32, String)> = program
            .cells
            .iter()
            .enumerate()
            .filter_map(|(idx, cell)| {
                let raw = idx as u32;
                ((raw >= inner_template_cell_range.0 && raw < inner_template_cell_range.1)
                    && (cell.name.contains("formula_text")
                        || cell.name.contains("display_value")
                        || cell.name.contains("comma_index")
                        || cell.name.contains("formula_length")
                        || cell.name == "expression"
                        || cell.name.contains("text_length")
                        || cell.name.contains("left_ref")
                        || cell.name.contains("right_ref")
                        || cell.name.contains("left_column")
                        || cell.name.contains("left_row")
                        || cell.name.contains("right_column")
                        || cell.name.contains("right_row")))
                .then_some((raw, cell.name.clone()))
            })
            .collect();
        let formula_text_cell = interesting_cells
            .iter()
            .find_map(|(raw, name)| name.contains("formula_text").then_some(CellId(*raw)))
            .expect("formula_text cell");
        let display_value_cell = interesting_cells
            .iter()
            .find_map(|(raw, name)| name.contains("display_value").then_some(CellId(*raw)))
            .expect("display_value cell");

        let second_bindings =
            list_map_bindings(&program, inner_map_cell, inner_item_cell, &inner_items[1]);
        let second_formula = resolve_runtime_cell_expr_with_bindings(
            &program,
            &cell_store,
            formula_text_cell,
            0,
            &second_bindings,
        )
        .expect("second formula");
        let second_value = resolve_runtime_cell_expr_with_bindings(
            &program,
            &cell_store,
            display_value_cell,
            0,
            &second_bindings,
        )
        .expect("second value");
        assert!(
            matches!(second_formula, IrExpr::Constant(IrValue::Text(ref t)) if t == "=add(A1, A2)"),
            "expected B1 formula to resolve to '=add(A1, A2)', got {second_formula:?}"
        );
        assert!(
            matches!(second_value, IrExpr::Constant(IrValue::Number(n)) if (n - 15.0).abs() < f64::EPSILON),
            "expected B1 display value to resolve to 15, got {second_value:?}"
        );

        let third_bindings =
            list_map_bindings(&program, inner_map_cell, inner_item_cell, &inner_items[2]);
        let third_formula = resolve_runtime_cell_expr_with_bindings(
            &program,
            &cell_store,
            formula_text_cell,
            0,
            &third_bindings,
        )
        .expect("third formula");
        let third_value = resolve_runtime_cell_expr_with_bindings(
            &program,
            &cell_store,
            display_value_cell,
            0,
            &third_bindings,
        )
        .expect("third value");
        assert!(
            matches!(third_formula, IrExpr::Constant(IrValue::Text(ref t)) if t == "=sum(A1:A3)"),
            "expected C1 formula to resolve to '=sum(A1:A3)', got {third_formula:?}"
        );
        assert!(
            matches!(third_value, IrExpr::Constant(IrValue::Number(n)) if (n - 30.0).abs() < f64::EPSILON),
            "expected C1 display value to resolve to 30, got {third_value:?}"
        );
    }

    #[test]
    fn cells_is_editing_resolves_true_only_for_first_cell_when_editing_a1() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/cells/cells.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");
        let cell_store = CellStore::new(program.cells.len());
        let list_store = ListStore::new();
        let template_list_items = Rc::new(RefCell::new(HashMap::new()));

        let template_ranges: Vec<(u32, u32)> = program
            .nodes
            .iter()
            .filter_map(|node| match node {
                IrNode::ListMap {
                    template_cell_range,
                    ..
                } => Some(*template_cell_range),
                _ => None,
            })
            .collect();
        let in_template_range = |cell: CellId| {
            template_ranges
                .iter()
                .any(|(start, end)| cell.0 >= *start && cell.0 < *end)
        };

        let editing_row_cell = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| {
                let cell_id = CellId(idx as u32);
                (cell.name == "editing_cell.row" && !in_template_range(cell_id)).then_some(cell_id)
            })
            .expect("global editing_cell.row");
        let editing_column_cell = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| {
                let cell_id = CellId(idx as u32);
                (cell.name == "editing_cell.column" && !in_template_range(cell_id))
                    .then_some(cell_id)
            })
            .expect("global editing_cell.column");
        cell_store.set_cell_f64(editing_row_cell.0, 1.0);
        cell_store.set_cell_f64(editing_column_cell.0, 1.0);

        let (inner_map_cell, inner_source_cell, inner_item_cell, inner_template_cell_range) =
            program
                .nodes
                .iter()
                .find_map(|node| match node {
                    IrNode::ListMap {
                        cell,
                        source,
                        item_name,
                        item_cell,
                        template_cell_range,
                        ..
                    } if item_name == "cell" => {
                        Some((*cell, *source, *item_cell, *template_cell_range))
                    }
                    _ => None,
                })
                .expect("inner row cell list map");

        let (outer_map_cell, outer_source_cell, outer_item_cell) = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::ListMap {
                    cell,
                    source,
                    item_name,
                    item_cell,
                    template_cell_range,
                    ..
                } if item_name == "row_data"
                    && inner_source_cell.0 >= template_cell_range.0
                    && inner_source_cell.0 < template_cell_range.1 =>
                {
                    Some((*cell, *source, *item_cell))
                }
                _ => None,
            })
            .expect("outer render row list map");

        let outer_item_expr =
            resolve_runtime_list_item_expr(&program, &cell_store, outer_map_cell, 0)
                .expect("first row item expr");
        let outer_bindings =
            list_map_bindings(&program, outer_map_cell, outer_item_cell, &outer_item_expr);
        let inner_source_expr = resolve_runtime_cell_expr_with_bindings(
            &program,
            &cell_store,
            inner_source_cell,
            0,
            &outer_bindings,
        )
        .expect("resolved first row cells expr");
        let inner_list_id = materialize_template_list_expr(
            &program,
            &list_store,
            &template_list_items,
            &cell_store,
            &inner_source_expr,
            0,
            &outer_bindings,
        )
        .expect("materialized first row cells");
        let inner_items = template_list_items
            .borrow()
            .get(&inner_list_id.to_bits())
            .cloned()
            .expect("inner items");

        let is_editing_cell = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| {
                let raw = idx as u32;
                (raw >= inner_template_cell_range.0
                    && raw < inner_template_cell_range.1
                    && cell.name == "is_editing")
                    .then_some(CellId(raw))
            })
            .expect("template is_editing cell");

        let first_bindings = list_map_bindings(
            &program,
            inner_map_cell,
            inner_item_cell,
            &inner_items.first().cloned().expect("first inner item"),
        );
        let second_bindings = list_map_bindings(
            &program,
            inner_map_cell,
            inner_item_cell,
            &inner_items.get(1).cloned().expect("second inner item"),
        );

        let first_resolved = resolve_runtime_cell_expr_with_bindings(
            &program,
            &cell_store,
            is_editing_cell,
            0,
            &first_bindings,
        )
        .expect("first is_editing");
        let second_resolved = resolve_runtime_cell_expr_with_bindings(
            &program,
            &cell_store,
            is_editing_cell,
            0,
            &second_bindings,
        )
        .expect("second is_editing");
        assert!(
            matches!(first_resolved, IrExpr::Constant(IrValue::Number(n)) if n == 1.0),
            "expected first cell is_editing == 1, got {first_resolved:?}"
        );
        assert!(
            matches!(second_resolved, IrExpr::Constant(IrValue::Number(n)) if n == 0.0),
            "expected second cell is_editing == 0, got {second_resolved:?}"
        );
    }

    #[test]
    fn cells_cell_template_global_deps_include_editing_state() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/cells/cells.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");

        let inner_template_cell_range = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::ListMap {
                    item_name,
                    template_cell_range,
                    ..
                } if item_name == "cell" => Some(*template_cell_range),
                _ => None,
            })
            .expect("render-time inner row cell list map");

        let deps = collect_template_global_dependencies(&program, inner_template_cell_range);
        let dep_names: Vec<String> = deps
            .iter()
            .filter_map(|cell| {
                program
                    .cells
                    .get(cell.0 as usize)
                    .map(|info| info.name.clone())
            })
            .collect();

        assert!(
            dep_names.iter().any(|name| name.contains("editing_cell")),
            "expected editing_cell in template global deps, got {dep_names:?}"
        );
        assert!(
            dep_names.iter().any(|name| name == "editing_cell.row"),
            "expected editing_cell.row in template global deps, got {dep_names:?}"
        );
        assert!(
            dep_names.iter().any(|name| name == "editing_cell.column"),
            "expected editing_cell.column in template global deps, got {dep_names:?}"
        );
        assert!(
            dep_names.iter().any(|name| name.contains("editing_text")),
            "expected editing_text in template global deps, got {dep_names:?}"
        );

        for dep in deps {
            let Some(info) = program.cells.get(dep.0 as usize) else {
                continue;
            };
            if info.name == "editing_cell.row" || info.name == "editing_cell.column" {
                let resolved = resolve_runtime_cell_expr_with_bindings(
                    &program,
                    &CellStore::new(program.cells.len()),
                    dep,
                    0,
                    &HashMap::new(),
                )
                .unwrap_or(IrExpr::CellRead(dep));
                assert!(
                    matches!(resolved, IrExpr::Constant(IrValue::Number(n)) if n == 0.0),
                    "expected canonical {name} dep to resolve to 0, got {resolved:?}",
                    name = info.name
                );
            }
        }
    }

    #[test]
    fn cells_row_template_cross_scope_events_include_cell_double_click() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/cells/cells.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");

        let inner_map_cell = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::ListMap {
                    cell,
                    source,
                    item_name,
                    ..
                } if item_name == "cell" && *source == CellId(112) => Some(*cell),
                _ => None,
            })
            .expect("edit_started inner row cell list map");

        let (_, template_cell_range, template_event_range) = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::ListMap {
                    item_name,
                    cell,
                    template_cell_range,
                    template_event_range,
                    ..
                } if item_name == "row_data"
                    && inner_map_cell.0 >= template_cell_range.0
                    && inner_map_cell.0 < template_cell_range.1 =>
                {
                    Some((*cell, *template_cell_range, *template_event_range))
                }
                _ => None,
            })
            .expect("edit_started outer row_data list map");

        let cross_scope_events =
            collect_cross_scope_events(&program, template_cell_range, template_event_range);
        let cross_scope_event_names: Vec<String> = cross_scope_events
            .iter()
            .filter_map(|event_id| {
                program
                    .events
                    .get(*event_id as usize)
                    .map(|event| event.name.clone())
            })
            .collect();
        assert!(
            cross_scope_event_names
                .iter()
                .any(|name| name.contains("display.event.double_click")),
            "expected cell double_click cross-scope event, got {cross_scope_event_names:?}"
        );
    }

    #[test]
    fn cells_fourth_row_first_cell_resolves_empty_formula_and_dot_label() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/cells/cells.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");
        let cell_store = CellStore::new(program.cells.len());
        let list_store = ListStore::new();
        let template_list_items = Rc::new(RefCell::new(HashMap::new()));

        let all_cell_maps: Vec<(CellId, CellId, CellId, (u32, u32))> = program
            .nodes
            .iter()
            .filter_map(|node| match node {
                IrNode::ListMap {
                    cell,
                    source,
                    item_name,
                    item_cell,
                    template_cell_range,
                    ..
                } if item_name == "cell" => {
                    Some((*cell, *source, *item_cell, *template_cell_range))
                }
                _ => None,
            })
            .collect();
        let (inner_map_cell, inner_source_cell, inner_item_cell, inner_template_cell_range) =
            all_cell_maps
                .iter()
                .find(|(_, source, _, _)| *source != CellId(0))
                .cloned()
                .expect("inner row cell list map");

        let (outer_map_cell, outer_source_cell, outer_item_cell) = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::ListMap {
                    cell,
                    source,
                    item_name,
                    item_cell,
                    template_cell_range,
                    ..
                } if item_name == "row_data"
                    && inner_map_cell.0 >= template_cell_range.0
                    && inner_map_cell.0 < template_cell_range.1 =>
                {
                    Some((*cell, *source, *item_cell))
                }
                _ => None,
            })
            .expect("outer render row list map");

        let fourth_row_item =
            resolve_runtime_list_item_expr(&program, &cell_store, outer_map_cell, 3)
                .expect("fourth row item expr");
        let outer_bindings =
            list_map_bindings(&program, outer_map_cell, outer_item_cell, &fourth_row_item);
        let inner_source_expr = resolve_runtime_cell_expr_with_bindings(
            &program,
            &cell_store,
            inner_source_cell,
            0,
            &outer_bindings,
        )
        .expect("resolved fourth row cells expr");
        let inner_list_id = materialize_template_list_expr(
            &program,
            &list_store,
            &template_list_items,
            &cell_store,
            &inner_source_expr,
            0,
            &outer_bindings,
        )
        .expect("materialized fourth row cells");
        let inner_items = template_list_items
            .borrow()
            .get(&inner_list_id.to_bits())
            .cloned()
            .expect("inner items");
        let first_cell_bindings = HashMap::from([(inner_item_cell, inner_items[0].clone())]);

        let formula_text_cell = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| {
                let raw = idx as u32;
                (raw >= inner_template_cell_range.0
                    && raw < inner_template_cell_range.1
                    && cell.name.contains("formula_text"))
                .then_some(CellId(raw))
            })
            .expect("formula_text cell");
        let display_value_cell = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| {
                let raw = idx as u32;
                (raw >= inner_template_cell_range.0
                    && raw < inner_template_cell_range.1
                    && cell.name.contains("display_value"))
                .then_some(CellId(raw))
            })
            .expect("display_value cell");
        let display_label_cell = program
            .nodes
            .iter()
            .filter_map(|node| match node {
                IrNode::Element {
                    cell,
                    kind: ElementKind::Label { label, .. },
                    links,
                    ..
                } if cell.0 >= inner_template_cell_range.0
                    && cell.0 < inner_template_cell_range.1
                    && links.iter().any(|(name, _)| name == "double_click") =>
                {
                    match label {
                        IrExpr::CellRead(label_cell) => Some(*label_cell),
                        _ => None,
                    }
                }
                _ => None,
            })
            .next()
            .expect("display label cell");

        let formula_text = resolve_runtime_cell_expr_with_bindings(
            &program,
            &cell_store,
            formula_text_cell,
            0,
            &first_cell_bindings,
        )
        .expect("resolved formula_text");
        let display_value = resolve_runtime_cell_expr_with_bindings(
            &program,
            &cell_store,
            display_value_cell,
            0,
            &first_cell_bindings,
        )
        .expect("resolved display_value");
        let display_label = resolve_runtime_cell_expr_with_bindings(
            &program,
            &cell_store,
            display_label_cell,
            0,
            &first_cell_bindings,
        )
        .expect("resolved display label");

        assert!(
            matches!(formula_text, IrExpr::Constant(IrValue::Text(ref t)) if t.is_empty()),
            "expected row 4 A formula text to resolve to empty text, got {formula_text:?}"
        );
        assert!(
            matches!(display_value, IrExpr::Constant(IrValue::Number(n)) if n.abs() < f64::EPSILON),
            "expected row 4 A display value to resolve to 0, got {display_value:?}"
        );
        assert!(
            matches!(display_label, IrExpr::Constant(IrValue::Text(ref t)) if t == "."),
            "expected row 4 A display label to resolve to '.', got {display_label:?}"
        );
    }

    #[test]
    fn cells_first_cell_seeding_populates_item_store_label_text() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/cells/cells.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");
        let cell_store = CellStore::new(program.cells.len());
        let list_store = ListStore::new();
        let template_list_items = Rc::new(RefCell::new(HashMap::new()));
        let template_ranges: Vec<(u32, u32)> = program
            .nodes
            .iter()
            .filter_map(|node| match node {
                IrNode::ListMap {
                    template_cell_range,
                    ..
                } => Some(*template_cell_range),
                _ => None,
            })
            .collect();
        let item_store = ItemCellStore::new(template_ranges);

        let all_cell_maps: Vec<(CellId, CellId, CellId, (u32, u32), String)> = program
            .nodes
            .iter()
            .filter_map(|node| match node {
                IrNode::ListMap {
                    cell,
                    source,
                    item_name,
                    item_cell,
                    template,
                    template_cell_range,
                    ..
                } if item_name == "cell" => Some((
                    *cell,
                    *source,
                    *item_cell,
                    *template_cell_range,
                    format!("{template:?}"),
                )),
                _ => None,
            })
            .collect();
        let (inner_map_cell, inner_source_cell, inner_item_cell, inner_template_cell_range, _) =
            all_cell_maps
                .iter()
                .find(|(_, source, _, _, _)| *source != CellId(0))
                .cloned()
                .expect("inner row cell list map");

        let (outer_map_cell, outer_source_cell, outer_item_cell) = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::ListMap {
                    cell,
                    source,
                    item_name,
                    item_cell,
                    template_cell_range,
                    ..
                } if item_name == "row_data"
                    && inner_map_cell.0 >= template_cell_range.0
                    && inner_map_cell.0 < template_cell_range.1 =>
                {
                    Some((*cell, *source, *item_cell))
                }
                _ => None,
            })
            .expect("outer render row list map");

        let outer_item_expr =
            resolve_runtime_list_item_expr(&program, &cell_store, outer_map_cell, 0)
                .expect("first row item expr");
        let outer_bindings =
            list_map_bindings(&program, outer_map_cell, outer_item_cell, &outer_item_expr);
        let inner_source_expr = resolve_runtime_cell_expr_with_bindings(
            &program,
            &cell_store,
            inner_source_cell,
            0,
            &outer_bindings,
        )
        .expect("resolved first row cells expr");
        let inner_list_id = materialize_template_list_expr(
            &program,
            &list_store,
            &template_list_items,
            &cell_store,
            &inner_source_expr,
            0,
            &outer_bindings,
        )
        .expect("materialized first row cells");
        let inner_items = template_list_items
            .borrow()
            .get(&inner_list_id.to_bits())
            .cloned()
            .expect("inner items");
        let first_cell_expr = inner_items.first().cloned().expect("first cell expr");

        let display_label_cell = program
            .nodes
            .iter()
            .filter_map(|node| match node {
                IrNode::Element {
                    cell,
                    kind: ElementKind::Label { label, .. },
                    links,
                    ..
                } if cell.0 >= inner_template_cell_range.0
                    && cell.0 < inner_template_cell_range.1
                    && links.iter().any(|(name, _)| name == "double_click") =>
                {
                    match label {
                        IrExpr::CellRead(label_cell) => Some(*label_cell),
                        _ => None,
                    }
                }
                _ => None,
            })
            .next()
            .expect("display label cell");

        item_store.ensure_item(0);
        let inner_plan = program.list_map_plan(inner_map_cell);
        let inner_template = inner_plan.map(|plan| &plan.template);
        let inner_resolved_item_store = inner_plan
            .map(|plan| plan.resolved_item_store)
            .unwrap_or(inner_item_cell);
        seed_item_template_cells_from_expr_parts(
            &program,
            &cell_store,
            &list_store,
            &template_list_items,
            &item_store,
            0,
            inner_template,
            inner_item_cell,
            inner_resolved_item_store,
            &first_cell_expr,
        );

        assert_eq!(
            item_store.get_text(0, display_label_cell.0),
            "5",
            "expected seeded first cell label text to be '5'"
        );
    }

    #[test]
    fn cells_first_cell_seeding_populates_bound_object_row_and_column_fields() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/cells/cells.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");
        let cell_store = CellStore::new(program.cells.len());
        let list_store = ListStore::new();
        let template_list_items = Rc::new(RefCell::new(HashMap::new()));

        let (inner_map_cell, inner_source_cell, inner_item_cell, inner_template_cell_range) =
            program
                .nodes
                .iter()
                .filter_map(|node| match node {
                    IrNode::ListMap {
                        cell,
                        source,
                        item_name,
                        item_cell,
                        template_cell_range,
                        ..
                    } if item_name == "cell" => {
                        Some((*cell, *source, *item_cell, *template_cell_range))
                    }
                    _ => None,
                })
                .next()
                .expect("inner row cell list map");

        let (outer_map_cell, _, outer_item_cell) = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::ListMap {
                    cell,
                    source,
                    item_name,
                    item_cell,
                    template_cell_range,
                    ..
                } if item_name == "row_data"
                    && inner_map_cell.0 >= template_cell_range.0
                    && inner_map_cell.0 < template_cell_range.1 =>
                {
                    Some((*cell, *source, *item_cell))
                }
                _ => None,
            })
            .expect("outer render row list map");

        let outer_item_expr =
            resolve_runtime_list_item_expr(&program, &cell_store, outer_map_cell, 0)
                .expect("first row item expr");
        let outer_bindings =
            list_map_bindings(&program, outer_map_cell, outer_item_cell, &outer_item_expr);
        let inner_source_expr = resolve_runtime_cell_expr_with_bindings(
            &program,
            &cell_store,
            inner_source_cell,
            0,
            &outer_bindings,
        )
        .expect("resolved first row cells expr");
        let inner_list_id = materialize_template_list_expr(
            &program,
            &list_store,
            &template_list_items,
            &cell_store,
            &inner_source_expr,
            0,
            &outer_bindings,
        )
        .expect("materialized first row cells");
        let inner_items = template_list_items
            .borrow()
            .get(&inner_list_id.to_bits())
            .cloned()
            .expect("inner items");

        let row_cell = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, info)| {
                let cell = CellId(idx as u32);
                (info.name == "object.row"
                    && cell.0 >= inner_template_cell_range.0
                    && cell.0 < inner_template_cell_range.1)
                    .then_some(cell)
            })
            .expect("object.row");
        let column_cell = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, info)| {
                let cell = CellId(idx as u32);
                (info.name == "object.column"
                    && cell.0 >= inner_template_cell_range.0
                    && cell.0 < inner_template_cell_range.1)
                    .then_some(cell)
            })
            .expect("object.column");

        let item_store = ItemCellStore::new(vec![inner_template_cell_range]);
        item_store.ensure_item(26);
        let inner_plan = program.list_map_plan(inner_map_cell);
        let inner_template = inner_plan.map(|plan| &plan.template);
        let inner_resolved_item_store = inner_plan
            .map(|plan| plan.resolved_item_store)
            .unwrap_or(inner_item_cell);
        seed_item_template_cells_from_expr_parts(
            &program,
            &cell_store,
            &list_store,
            &template_list_items,
            &item_store,
            26,
            inner_template,
            inner_item_cell,
            inner_resolved_item_store,
            inner_items.first().expect("first cell item"),
        );

        assert_eq!(
            item_store.get_value(26, row_cell.0),
            1.0,
            "expected seeded first cell object.row to be 1"
        );
        assert_eq!(
            item_store.get_value(26, column_cell.0),
            1.0,
            "expected seeded first cell object.column to be 1"
        );
    }

    #[test]
    fn cells_first_cell_active_root_uses_display_label_initially() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/cells/cells.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");
        let cell_store = CellStore::new(program.cells.len());
        let list_store = ListStore::new();
        let template_list_items = Rc::new(RefCell::new(HashMap::new()));

        let (inner_map_cell, inner_source_cell, inner_item_cell, inner_template_cell_range) =
            program
                .nodes
                .iter()
                .filter_map(|node| match node {
                    IrNode::ListMap {
                        cell,
                        source,
                        item_name,
                        item_cell,
                        template_cell_range,
                        ..
                    } if item_name == "cell" => {
                        Some((*cell, *source, *item_cell, *template_cell_range))
                    }
                    _ => None,
                })
                .next()
                .expect("inner row cell list map");

        let (outer_map_cell, outer_source_cell, outer_item_cell) = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::ListMap {
                    cell,
                    source,
                    item_name,
                    item_cell,
                    template_cell_range,
                    ..
                } if item_name == "row_data"
                    && inner_map_cell.0 >= template_cell_range.0
                    && inner_map_cell.0 < template_cell_range.1 =>
                {
                    Some((*cell, *source, *item_cell))
                }
                _ => None,
            })
            .expect("outer render row list map");

        let outer_item_expr =
            resolve_runtime_list_item_expr(&program, &cell_store, outer_map_cell, 0)
                .expect("first row item expr");
        let outer_bindings =
            list_map_bindings(&program, outer_map_cell, outer_item_cell, &outer_item_expr);
        let inner_source_expr = resolve_runtime_cell_expr_with_bindings(
            &program,
            &cell_store,
            inner_source_cell,
            0,
            &outer_bindings,
        )
        .expect("resolved first row cells expr");
        let inner_list_id = materialize_template_list_expr(
            &program,
            &list_store,
            &template_list_items,
            &cell_store,
            &inner_source_expr,
            0,
            &outer_bindings,
        )
        .expect("materialized first row cells");
        let inner_items = template_list_items
            .borrow()
            .get(&inner_list_id.to_bits())
            .cloned()
            .expect("inner items");
        let first_cell_expr = inner_items.first().cloned().expect("first cell expr");

        let mut bindings = HashMap::new();
        bindings.insert(inner_item_cell, first_cell_expr);

        let inner_template = program
            .list_map_plan(inner_map_cell)
            .map(|plan| &plan.template);
        let root_cell = inner_template
            .and_then(|template| template.root_cell)
            .expect("inner template root cell");

        let active_root = resolve_active_template_root_cell(
            &program,
            &cell_store,
            root_cell,
            inner_template_cell_range,
            &bindings,
            &mut HashSet::new(),
        )
        .expect("active root");

        match find_node_for_cell(&program, active_root) {
            Some(IrNode::Element {
                kind: ElementKind::Label { .. },
                ..
            }) => {}
            other => panic!("expected initial active root to be display label, got {other:?}"),
        }

        let seed_cells =
            template_seed_cells_for_active_root(&program, &cell_store, inner_template, &bindings);
        for seed_cell in seed_cells {
            let Some(info) = program.cells.get(seed_cell.0 as usize) else {
                continue;
            };
            if info.name == "editing_cell.row" || info.name == "editing_cell.column" {
                let resolved = resolve_runtime_cell_expr_with_bindings(
                    &program,
                    &cell_store,
                    seed_cell,
                    0,
                    &bindings,
                )
                .unwrap_or(IrExpr::CellRead(seed_cell));
                assert!(
                    matches!(resolved, IrExpr::Constant(IrValue::Number(n)) if n == 0.0),
                    "expected canonical seed {name} to resolve to 0, got {resolved:?}",
                    name = info.name
                );
            }
        }
    }

    #[test]
    fn cells_active_root_only_switches_first_cell_when_editing_cell_is_a1() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/cells/cells.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");
        let cell_store = CellStore::new(program.cells.len());
        let list_store = ListStore::new();
        let template_list_items = Rc::new(RefCell::new(HashMap::new()));

        let template_ranges: Vec<(u32, u32)> = program
            .nodes
            .iter()
            .filter_map(|node| match node {
                IrNode::ListMap {
                    template_cell_range,
                    ..
                } => Some(*template_cell_range),
                _ => None,
            })
            .collect();
        let in_template_range = |cell: CellId| {
            template_ranges
                .iter()
                .any(|(start, end)| cell.0 >= *start && cell.0 < *end)
        };

        let editing_row_cell = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| {
                let cell_id = CellId(idx as u32);
                (cell.name == "editing_cell.row" && !in_template_range(cell_id)).then_some(cell_id)
            })
            .expect("global editing_cell.row");
        let editing_column_cell = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| {
                let cell_id = CellId(idx as u32);
                (cell.name == "editing_cell.column" && !in_template_range(cell_id))
                    .then_some(cell_id)
            })
            .expect("global editing_cell.column");

        cell_store.set_cell_f64(editing_row_cell.0, 1.0);
        cell_store.set_cell_f64(editing_column_cell.0, 1.0);

        let (inner_map_cell, inner_source_cell, inner_item_cell, inner_template_cell_range) =
            program
                .nodes
                .iter()
                .filter_map(|node| match node {
                    IrNode::ListMap {
                        cell,
                        source,
                        item_name,
                        item_cell,
                        template_cell_range,
                        ..
                    } if item_name == "cell" => {
                        Some((*cell, *source, *item_cell, *template_cell_range))
                    }
                    _ => None,
                })
                .next()
                .expect("inner row cell list map");

        let (outer_map_cell, outer_source_cell, outer_item_cell) = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::ListMap {
                    cell,
                    source,
                    item_name,
                    item_cell,
                    template_cell_range,
                    ..
                } if item_name == "row_data"
                    && inner_map_cell.0 >= template_cell_range.0
                    && inner_map_cell.0 < template_cell_range.1 =>
                {
                    Some((*cell, *source, *item_cell))
                }
                _ => None,
            })
            .expect("outer render row list map");

        let outer_item_expr =
            resolve_runtime_list_item_expr(&program, &cell_store, outer_map_cell, 0)
                .expect("first row item expr");
        let outer_bindings =
            list_map_bindings(&program, outer_map_cell, outer_item_cell, &outer_item_expr);
        let inner_source_expr = resolve_runtime_cell_expr_with_bindings(
            &program,
            &cell_store,
            inner_source_cell,
            0,
            &outer_bindings,
        )
        .expect("resolved first row cells expr");
        let inner_list_id = materialize_template_list_expr(
            &program,
            &list_store,
            &template_list_items,
            &cell_store,
            &inner_source_expr,
            0,
            &outer_bindings,
        )
        .expect("materialized first row cells");
        let inner_items = template_list_items
            .borrow()
            .get(&inner_list_id.to_bits())
            .cloned()
            .expect("inner items");

        let root_cell = program
            .list_map_plan(inner_map_cell)
            .and_then(|plan| plan.template.root_cell)
            .expect("inner template root cell");

        let resolve_active_root_for_item = |item_expr: IrExpr| {
            let bindings = list_map_bindings(&program, inner_map_cell, inner_item_cell, &item_expr);
            resolve_active_template_root_cell(
                &program,
                &cell_store,
                root_cell,
                inner_template_cell_range,
                &bindings,
                &mut HashSet::new(),
            )
            .expect("active root")
        };

        let first_active_root =
            resolve_active_root_for_item(inner_items.first().cloned().expect("first item"));
        let second_active_root =
            resolve_active_root_for_item(inner_items.get(1).cloned().expect("second item"));

        match find_node_for_cell(&program, first_active_root) {
            Some(IrNode::Element {
                kind: ElementKind::TextInput { .. },
                ..
            }) => {}
            other => panic!("expected first cell to switch to text input, got {other:?}"),
        }

        match find_node_for_cell(&program, second_active_root) {
            Some(IrNode::Element {
                kind: ElementKind::Label { .. },
                ..
            }) => {}
            other => {
                panic!("expected second cell to remain display label, got {other:?}")
            }
        }
    }

    #[test]
    fn cells_is_editing_source_seeds_true_only_for_first_cell_when_editing_a1() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/cells/cells.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");
        let cell_store = CellStore::new(program.cells.len());
        let list_store = ListStore::new();
        let template_list_items = Rc::new(RefCell::new(HashMap::new()));
        let template_ranges: Vec<(u32, u32)> = program
            .nodes
            .iter()
            .filter_map(|node| match node {
                IrNode::ListMap {
                    template_cell_range,
                    ..
                } => Some(*template_cell_range),
                _ => None,
            })
            .collect();
        let item_store = ItemCellStore::new(template_ranges.clone());

        let in_template_range = |cell: CellId| {
            template_ranges
                .iter()
                .any(|(start, end)| cell.0 >= *start && cell.0 < *end)
        };

        let editing_row_cell = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| {
                let cell_id = CellId(idx as u32);
                (cell.name == "editing_cell.row" && !in_template_range(cell_id)).then_some(cell_id)
            })
            .expect("global editing_cell.row");
        let editing_column_cell = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| {
                let cell_id = CellId(idx as u32);
                (cell.name == "editing_cell.column" && !in_template_range(cell_id))
                    .then_some(cell_id)
            })
            .expect("global editing_cell.column");

        cell_store.set_cell_f64(editing_row_cell.0, 1.0);
        cell_store.set_cell_f64(editing_column_cell.0, 1.0);

        let (inner_map_cell, inner_source_cell, inner_item_cell, inner_template_cell_range) =
            program
                .nodes
                .iter()
                .filter_map(|node| match node {
                    IrNode::ListMap {
                        cell,
                        source,
                        item_name,
                        item_cell,
                        template_cell_range,
                        ..
                    } if item_name == "cell" => {
                        Some((*cell, *source, *item_cell, *template_cell_range))
                    }
                    _ => None,
                })
                .next()
                .expect("inner row cell list map");

        let root_source_cell = match find_node_for_cell(
            &program,
            program
                .list_map_plan(inner_map_cell)
                .and_then(|plan| plan.template.root_cell)
                .expect("inner template root cell"),
        ) {
            Some(IrNode::While { source, .. }) | Some(IrNode::When { source, .. }) => *source,
            other => panic!("expected cell template root selector, got {other:?}"),
        };

        let (outer_map_cell, outer_source_cell, outer_item_cell) = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::ListMap {
                    cell,
                    source,
                    item_name,
                    item_cell,
                    template_cell_range,
                    ..
                } if item_name == "row_data"
                    && inner_map_cell.0 >= template_cell_range.0
                    && inner_map_cell.0 < template_cell_range.1 =>
                {
                    Some((*cell, *source, *item_cell))
                }
                _ => None,
            })
            .expect("outer render row list map");

        let outer_item_expr =
            resolve_runtime_list_item_expr(&program, &cell_store, outer_map_cell, 0)
                .expect("first row item expr");
        let outer_bindings =
            list_map_bindings(&program, outer_map_cell, outer_item_cell, &outer_item_expr);
        let inner_source_expr = resolve_runtime_cell_expr_with_bindings(
            &program,
            &cell_store,
            inner_source_cell,
            0,
            &outer_bindings,
        )
        .expect("resolved first row cells expr");
        let inner_list_id = materialize_template_list_expr(
            &program,
            &list_store,
            &template_list_items,
            &cell_store,
            &inner_source_expr,
            0,
            &outer_bindings,
        )
        .expect("materialized first row cells");
        let inner_items = template_list_items
            .borrow()
            .get(&inner_list_id.to_bits())
            .cloned()
            .expect("inner items");

        item_store.ensure_item(0);
        let inner_plan = program.list_map_plan(inner_map_cell);
        let inner_template = inner_plan.map(|plan| &plan.template);
        let inner_resolved_item_store = inner_plan
            .map(|plan| plan.resolved_item_store)
            .unwrap_or(inner_item_cell);
        seed_item_template_cells_from_expr_parts(
            &program,
            &cell_store,
            &list_store,
            &template_list_items,
            &item_store,
            0,
            inner_template,
            inner_item_cell,
            inner_resolved_item_store,
            inner_items.first().expect("first cell item"),
        );

        item_store.ensure_item(1);
        seed_item_template_cells_from_expr_parts(
            &program,
            &cell_store,
            &list_store,
            &template_list_items,
            &item_store,
            1,
            inner_template,
            inner_item_cell,
            inner_resolved_item_store,
            inner_items.get(1).expect("second cell item"),
        );

        let first_bindings = list_map_bindings(
            &program,
            inner_map_cell,
            inner_item_cell,
            inner_items.first().expect("first cell item"),
        );
        let second_bindings = list_map_bindings(
            &program,
            inner_map_cell,
            inner_item_cell,
            inner_items.get(1).expect("second cell item"),
        );
        assert_eq!(
            item_store.get_value(0, root_source_cell.0),
            1.0,
            "expected first cell is_editing source to seed true"
        );
        assert_eq!(
            item_store.get_value(1, root_source_cell.0),
            0.0,
            "expected second cell is_editing source to seed false"
        );
    }

    #[test]
    fn cells_outer_row_seeding_keeps_distinct_row_numbers() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/cells/cells.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");
        let cell_store = CellStore::new(program.cells.len());
        let list_store = ListStore::new();
        let template_list_items = Rc::new(RefCell::new(HashMap::new()));
        let template_ranges: Vec<(u32, u32)> = program
            .nodes
            .iter()
            .filter_map(|node| match node {
                IrNode::ListMap {
                    template_cell_range,
                    ..
                } => Some(*template_cell_range),
                _ => None,
            })
            .collect();
        let item_store = ItemCellStore::new(template_ranges);

        let (outer_map_cell, outer_source_cell, outer_item_cell, outer_template_cell_range) =
            program
                .nodes
                .iter()
                .find_map(|node| match node {
                    IrNode::ListMap {
                        cell,
                        source,
                        item_name,
                        item_cell,
                        template_cell_range,
                        ..
                    } if item_name == "row_data" => {
                        Some((*cell, *source, *item_cell, *template_cell_range))
                    }
                    _ => None,
                })
                .expect("outer row list map");

        let first_row_item =
            resolve_runtime_list_item_expr(&program, &cell_store, outer_map_cell, 0)
                .expect("first row item expr");
        let outer_row_cell = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, info)| {
                let cell = CellId(idx as u32);
                (info.name == "row_data.row"
                    && cell.0 >= outer_template_cell_range.0
                    && cell.0 < outer_template_cell_range.1)
                    .then_some(cell)
            })
            .expect("row_data.row in outer template");
        let outer_first_cell_row = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, info)| {
                let cell = CellId(idx as u32);
                (info.name == "row_data.cells.row"
                    && cell.0 >= outer_template_cell_range.0
                    && cell.0 < outer_template_cell_range.1)
                    .then_some(cell)
            })
            .expect("row_data.cells.row in outer template");
        let outer_first_cell_column = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, info)| {
                let cell = CellId(idx as u32);
                (info.name == "row_data.cells.column"
                    && cell.0 >= outer_template_cell_range.0
                    && cell.0 < outer_template_cell_range.1)
                    .then_some(cell)
            })
            .expect("row_data.cells.column in outer template");

        item_store.ensure_item(0);
        let outer_plan = program.list_map_plan(outer_map_cell);
        let outer_template = outer_plan.map(|plan| &plan.template);
        let outer_resolved_item_store = outer_plan
            .map(|plan| plan.resolved_item_store)
            .unwrap_or(outer_item_cell);
        seed_item_template_cells_from_expr_parts(
            &program,
            &cell_store,
            &list_store,
            &template_list_items,
            &item_store,
            0,
            outer_template,
            outer_item_cell,
            outer_resolved_item_store,
            &first_row_item,
        );

        assert_eq!(
            item_store.get_value(0, outer_row_cell.0),
            1.0,
            "expected seeded first outer row number to be 1"
        );
        assert_eq!(
            item_store.get_value(0, outer_first_cell_row.0),
            1.0,
            "expected seeded first visible cell row to be 1 in outer row template"
        );
        assert_eq!(
            item_store.get_value(0, outer_first_cell_column.0),
            1.0,
            "expected seeded first visible cell column to be 1 in outer row template"
        );

        let second_row_item =
            resolve_runtime_list_item_expr(&program, &cell_store, outer_map_cell, 1)
                .expect("second row item expr");

        item_store.ensure_item(1);
        seed_item_template_cells_from_expr_parts(
            &program,
            &cell_store,
            &list_store,
            &template_list_items,
            &item_store,
            1,
            outer_template,
            outer_item_cell,
            outer_resolved_item_store,
            &second_row_item,
        );

        assert_eq!(
            item_store.get_value(1, outer_row_cell.0),
            2.0,
            "expected seeded second outer row number to be 2"
        );
        assert_eq!(
            item_store.get_value(1, outer_first_cell_row.0),
            2.0,
            "expected seeded second row visible cell row to be 2 in outer row template"
        );
        assert_eq!(
            item_store.get_value(1, outer_first_cell_column.0),
            1.0,
            "expected seeded second row visible cell column to remain 1 in outer row template"
        );
    }

    #[test]
    fn cells_outer_row_local_event_seed_cells_include_first_cell_coordinates() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/cells/cells.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");

        let (outer_map_cell, _, _, outer_template_cell_range, _outer_template_event_range) =
            program
                .nodes
                .iter()
                .find_map(|node| match node {
                    IrNode::ListMap {
                        cell,
                        source,
                        item_name,
                        item_cell,
                        template_cell_range,
                        template_event_range,
                        ..
                    } if item_name == "row_data" => Some((
                        *cell,
                        *source,
                        *item_cell,
                        *template_cell_range,
                        *template_event_range,
                    )),
                    _ => None,
                })
                .expect("outer row list map");

        let outer_row_cell = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, info)| {
                let cell = CellId(idx as u32);
                (info.name == "row_data.cells.row"
                    && cell.0 >= outer_template_cell_range.0
                    && cell.0 < outer_template_cell_range.1)
                    .then_some(cell)
            })
            .expect("row_data.cells.row in outer template");
        let outer_column_cell = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, info)| {
                let cell = CellId(idx as u32);
                (info.name == "row_data.cells.column"
                    && cell.0 >= outer_template_cell_range.0
                    && cell.0 < outer_template_cell_range.1)
                    .then_some(cell)
            })
            .expect("row_data.cells.column in outer template");

        let outer_plan = program
            .list_map_plan(outer_map_cell)
            .expect("outer row plan");
        let seed_cells = template_seed_cells_for_local_events(&program, Some(&outer_plan.template));

        assert!(
            seed_cells.contains(&outer_row_cell),
            "expected local-event seed cells to include row_data.cells.row, got {seed_cells:?}"
        );
        assert!(
            seed_cells.contains(&outer_column_cell),
            "expected local-event seed cells to include row_data.cells.column, got {seed_cells:?}"
        );
    }

    #[test]
    fn cells_first_cell_edit_started_runtime_shape_is_inspectable() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/cells/cells.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");
        let cell_store = CellStore::new(program.cells.len());
        let list_store = ListStore::new();
        let template_list_items = Rc::new(RefCell::new(HashMap::new()));

        let (inner_map_cell, inner_source_cell, inner_item_cell, inner_template_cell_range) =
            program
                .nodes
                .iter()
                .filter_map(|node| match node {
                    IrNode::ListMap {
                        cell,
                        source,
                        item_name,
                        item_cell,
                        template_cell_range,
                        ..
                    } if item_name == "cell" => {
                        Some((*cell, *source, *item_cell, *template_cell_range))
                    }
                    _ => None,
                })
                .next()
                .expect("inner row cell list map");

        let (outer_map_cell, outer_source_cell, outer_item_cell) = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::ListMap {
                    cell,
                    source,
                    item_name,
                    item_cell,
                    template_cell_range,
                    ..
                } if item_name == "row_data"
                    && inner_map_cell.0 >= template_cell_range.0
                    && inner_map_cell.0 < template_cell_range.1 =>
                {
                    Some((*cell, *source, *item_cell))
                }
                _ => None,
            })
            .expect("outer render row list map");

        let outer_item_expr =
            resolve_runtime_list_item_expr(&program, &cell_store, outer_map_cell, 0)
                .expect("first row item expr");
        let outer_bindings =
            list_map_bindings(&program, outer_map_cell, outer_item_cell, &outer_item_expr);
        let inner_source_expr = resolve_runtime_cell_expr_with_bindings(
            &program,
            &cell_store,
            inner_source_cell,
            0,
            &outer_bindings,
        )
        .expect("resolved first row cells expr");
        let inner_list_id = materialize_template_list_expr(
            &program,
            &list_store,
            &template_list_items,
            &cell_store,
            &inner_source_expr,
            0,
            &outer_bindings,
        )
        .expect("materialized first row cells");
        let inner_items = template_list_items
            .borrow()
            .get(&inner_list_id.to_bits())
            .cloned()
            .expect("inner items");
        let first_cell_expr = inner_items.first().cloned().expect("first cell expr");
        let inner_bindings = HashMap::from([(inner_item_cell, first_cell_expr)]);

        let interesting_cells: Vec<(CellId, String)> = program
            .cells
            .iter()
            .enumerate()
            .filter_map(|(idx, cell)| {
                let raw = idx as u32;
                (raw >= inner_template_cell_range.0
                    && raw < inner_template_cell_range.1
                    && (cell.name.contains("edit_started")
                        || cell.name.contains("editing_cell")
                        || cell.name.contains("display.event.double_click")))
                .then_some((CellId(raw), cell.name.clone()))
            })
            .collect();

        let global_edit_cells: Vec<(u32, String)> = program
            .cells
            .iter()
            .enumerate()
            .filter_map(|(idx, cell)| {
                (cell.name.contains("edit_started")
                    || cell.name.contains("editing_cell")
                    || cell.name.contains("editing_text"))
                .then_some((idx as u32, cell.name.clone()))
            })
            .collect();
        assert!(
            !global_edit_cells.is_empty(),
            "expected global edit state cells"
        );
    }

    #[test]
    fn cells_first_cell_double_click_event_matches_edit_started_arm_trigger() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/cells/cells.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");

        let display_double_click_event = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::Element { links, .. } => links
                    .iter()
                    .find_map(|(name, event)| (name == "double_click").then_some(*event)),
                _ => None,
            })
            .expect("cell display double_click event");

        let edit_started_trigger = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::Latest { target, arms } if *target == CellId(0) => {
                    arms.first().and_then(|arm| arm.trigger)
                }
                _ => None,
            })
            .expect("edit_started latest trigger");

        assert_eq!(
            display_double_click_event.0, edit_started_trigger.0,
            "expected first cell display double_click event id to match edit_started trigger"
        );
    }
}
