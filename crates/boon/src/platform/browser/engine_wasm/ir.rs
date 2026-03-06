//! Reactive IR types for the WASM compilation engine.
//!
//! The IR sits between the parsed AST and WASM codegen. It represents
//! the reactive program as a graph of cells (stateful slots) and events
//! (triggers), connected by nodes (reactive operators).

use std::collections::HashMap;

use boon_scene::{RenderRootHandle, RenderSurface};

use crate::parser::Span;

// ---------------------------------------------------------------------------
// Identifiers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CellId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EventId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FuncId(pub u32);

// ---------------------------------------------------------------------------
// Expressions (evaluated to produce values)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum IrExpr {
    Constant(IrValue),
    CellRead(CellId),
    FieldAccess {
        object: Box<IrExpr>,
        field: String,
    },
    BinOp {
        op: BinOp,
        lhs: Box<IrExpr>,
        rhs: Box<IrExpr>,
    },
    UnaryNeg(Box<IrExpr>),
    Compare {
        op: CmpOp,
        lhs: Box<IrExpr>,
        rhs: Box<IrExpr>,
    },
    TextConcat(Vec<TextSegment>),
    FunctionCall {
        func: FuncId,
        args: Vec<IrExpr>,
    },
    /// Boolean NOT: flips 1.0 ↔ 0.0.
    Not(Box<IrExpr>),
    ObjectConstruct(Vec<(String, IrExpr)>),
    ListConstruct(Vec<IrExpr>),
    TaggedObject {
        tag: String,
        fields: Vec<(String, IrExpr)>,
    },
    /// Inline pattern match expression (used in HOLD bodies with WHEN).
    /// Reads source cell, matches patterns, returns corresponding body value.
    /// If no arm matches (or SKIP), produces the SKIP sentinel NaN.
    PatternMatch {
        source: CellId,
        arms: Vec<(IrPattern, IrExpr)>,
    },
}

#[derive(Debug, Clone)]
pub enum TextSegment {
    Literal(String),
    Expr(IrExpr),
}

#[derive(Debug, Clone)]
pub enum IrValue {
    Number(f64),
    Text(String),
    Bool(bool),
    Tag(String),
    Object(Vec<(String, IrValue)>),
    Void,
    /// SKIP sentinel: signals that WHEN/WHILE should not update the target cell.
    /// Encoded as a specific NaN value in WASM.
    Skip,
}

/// Special f64 bit pattern used as SKIP sentinel in WASM.
/// Using a specific NaN payload that won't occur in normal arithmetic.
/// We compare this exact bit pattern via i64.reinterpret_f64 + i64.eq in codegen
/// to detect SKIP. Do NOT use generic NaN checks (f64.ne with self) because
/// text-only cells have regular NaN f64 values that should not be treated as SKIP.
pub const SKIP_SENTINEL_BITS: u64 = 0x7FF8_0000_0000_0001; // quiet NaN with payload 1

#[derive(Debug, Clone, Copy)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Debug, Clone, Copy)]
pub enum CmpOp {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
}

// ---------------------------------------------------------------------------
// Top-level reactive nodes
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum IrNode {
    /// Derived cell: value computed from expression (no external trigger).
    Derived { cell: CellId, expr: IrExpr },

    /// HOLD: mutable cell with initial value, updated on triggers.
    /// Each trigger has its own body expression (e.g., LATEST inside HOLD
    /// may have different THEN bodies per arm).
    Hold {
        cell: CellId,
        init: IrExpr,
        trigger_bodies: Vec<(EventId, IrExpr)>,
    },

    /// LATEST: multiple arms updating the same target cell.
    Latest {
        target: CellId,
        arms: Vec<LatestArm>,
    },

    /// THEN: evaluate body when trigger fires.
    Then {
        cell: CellId,
        trigger: EventId,
        body: IrExpr,
    },

    /// WHEN: pattern match on source cell (re-eval only when source changes).
    When {
        cell: CellId,
        source: CellId,
        arms: Vec<(IrPattern, IrExpr)>,
    },

    /// WHILE: pattern match that re-evals on source OR dependency changes.
    While {
        cell: CellId,
        source: CellId,
        deps: Vec<CellId>,
        arms: Vec<(IrPattern, IrExpr)>,
    },

    /// Timer: periodic event source.
    Timer { event: EventId, interval_ms: IrExpr },

    /// Element with LINK bindings.
    Element {
        cell: CellId,
        kind: ElementKind,
        links: Vec<(String, EventId)>,
        /// Cell that tracks hover state (True/False). Set by the bridge.
        hovered_cell: Option<CellId>,
    },

    /// Render root lowered from Document/new or Scene/new.
    Document {
        kind: RenderSurface,
        root: CellId,
        lights: Option<CellId>,
        geometry: Option<CellId>,
    },

    /// TEXT interpolation output cell.
    TextInterpolation {
        cell: CellId,
        parts: Vec<TextSegment>,
    },

    /// Math/sum accumulator.
    MathSum { cell: CellId, input: CellId },

    /// Pipe from one cell to another (identity / pass-through).
    PipeThrough { cell: CellId, source: CellId },

    /// Skip the first N emissions from a source stream.
    StreamSkip {
        cell: CellId,
        source: CellId,
        count: usize,
        seen_cell: CellId,
    },

    /// FunctionCall that doesn't map to a known built-in — placeholder for
    /// user-defined functions or not-yet-implemented built-ins.
    CustomCall {
        cell: CellId,
        path: Vec<String>,
        args: Vec<(String, IrExpr)>,
    },

    /// List append: add item to list when trigger fires.
    /// `watch_cell`: optional cell whose changes also trigger append (for reactive item expressions).
    ListAppend {
        cell: CellId,
        source: CellId,
        item: CellId,
        trigger: EventId,
        watch_cell: Option<CellId>,
    },

    /// List clear: remove all items when trigger fires.
    ListClear {
        cell: CellId,
        source: CellId,
        trigger: EventId,
    },

    /// List count: output f64 count of items in source list.
    ListCount { cell: CellId, source: CellId },

    /// List map: transform each item to an element.
    ListMap {
        cell: CellId,
        source: CellId,
        item_name: String,
        item_cell: CellId,
        template: Box<IrNode>,
        /// CellId range [start, end) for template-scoped cells.
        template_cell_range: (u32, u32),
        /// EventId range [start, end) for template-scoped events.
        template_event_range: (u32, u32),
    },

    /// List remove: remove item(s) when trigger fires.
    /// When `predicate` is Some, uses a per-item filter loop (inverted retain):
    /// items where predicate is truthy are removed, others are kept.
    /// When `predicate` is None, the trigger is a per-item event and the
    /// specific item that fired the event is removed.
    ListRemove {
        cell: CellId,
        source: CellId,
        trigger: EventId,
        predicate: Option<CellId>,
        /// The item placeholder cell (used as iteration variable in filter loop).
        item_cell: Option<CellId>,
        /// Field sub-cells for per-item evaluation (e.g., [("completed", sub_cell_id)]).
        item_field_cells: Vec<(String, CellId)>,
    },

    /// List retain: filter items based on predicate.
    /// When `item_cell` and `item_field_cells` are present, per-item filtering is used:
    /// the WASM filter loop iterates items, reads per-item field values from linear
    /// memory, evaluates the predicate for each item, and builds a filtered list.
    ListRetain {
        cell: CellId,
        source: CellId,
        predicate: Option<CellId>,
        /// The item placeholder cell (used as iteration variable in filter loop).
        item_cell: Option<CellId>,
        /// Field sub-cells for per-item evaluation (e.g., [("completed", sub_cell_id)]).
        item_field_cells: Vec<(String, CellId)>,
    },

    /// List every: check if all items match predicate. Output bool (1.0/0.0).
    /// Uses the same per-item iteration pattern as ListRetain but produces a boolean.
    ListEvery {
        cell: CellId,
        source: CellId,
        predicate: Option<CellId>,
        item_cell: Option<CellId>,
        item_field_cells: Vec<(String, CellId)>,
    },

    /// List any: check if any item matches predicate. Output bool (1.0/0.0).
    ListAny {
        cell: CellId,
        source: CellId,
        predicate: Option<CellId>,
        item_cell: Option<CellId>,
        item_field_cells: Vec<(String, CellId)>,
    },

    /// List is_empty: output bool (1.0/0.0) whether list is empty.
    ListIsEmpty { cell: CellId, source: CellId },

    /// Router/go_to: navigate to a URL.
    RouterGoTo { cell: CellId, source: CellId },

    /// Text/trim: trim whitespace.
    TextTrim { cell: CellId, source: CellId },

    /// Text/is_not_empty: check if text is non-empty.
    TextIsNotEmpty { cell: CellId, source: CellId },

    /// Text/to_number: parse text to number. Returns NaN tag index if invalid.
    TextToNumber { cell: CellId, source: CellId, nan_tag_value: f64 },

    /// Text/starts_with: check if source text starts with prefix text.
    TextStartsWith { cell: CellId, source: CellId, prefix: CellId },

    /// Math/round: round number to nearest integer.
    MathRound { cell: CellId, source: CellId },

    /// Math/min: clamp source to at most `b`.
    MathMin {
        cell: CellId,
        source: CellId,
        b: CellId,
    },

    /// Math/max: clamp source to at least `b`.
    MathMax {
        cell: CellId,
        source: CellId,
        b: CellId,
    },

    /// HOLD with object state and Stream/pulses loop.
    /// Computes N iterations at init time, updating per-field cells.
    /// Used for patterns like:
    /// ```text
    /// [a: 0, b: 1] |> HOLD state {
    ///     N |> Stream/pulses() |> THEN {
    ///         [a: state.b, b: state.a + state.b]
    ///     }
    /// }
    /// ```
    HoldLoop {
        cell: CellId,
        field_cells: Vec<(String, CellId)>,
        init_values: Vec<(String, IrExpr)>,
        count_expr: IrExpr,
        body_fields: Vec<(String, IrExpr)>,
    },
}

#[derive(Clone, Debug)]
pub struct LatestArm {
    pub trigger: Option<EventId>,
    pub body: IrExpr,
}

// ---------------------------------------------------------------------------
// Patterns
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum IrPattern {
    Number(f64),
    Text(String),
    Tag(String),
    Wildcard,
    Binding(String),
}

// ---------------------------------------------------------------------------
// Elements
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum ElementKind {
    Button {
        label: IrExpr,
        style: IrExpr,
    },
    TextInput {
        placeholder: Option<IrExpr>,
        style: IrExpr,
        focus: bool,
        /// Cell providing the reactive text value (e.g., a LATEST target cell).
        /// Used by the bridge to set the initial input value.
        text_cell: Option<CellId>,
    },
    Checkbox {
        checked: Option<CellId>,
        style: IrExpr,
        icon: Option<CellId>,
    },
    Stripe {
        direction: IrExpr,
        items: CellId,
        gap: IrExpr,
        style: IrExpr,
        element_settings: IrExpr,
    },
    Container {
        child: CellId,
        style: IrExpr,
    },
    Label {
        label: IrExpr,
        style: IrExpr,
    },
    Stack {
        layers: CellId,
        style: IrExpr,
    },
    Link {
        url: IrExpr,
        label: IrExpr,
        style: IrExpr,
    },
    Paragraph {
        content: IrExpr,
        style: IrExpr,
    },
    Block {
        child: CellId,
        style: IrExpr,
    },
    Text {
        label: IrExpr,
        style: IrExpr,
    },
    Slider {
        style: IrExpr,
        value_cell: Option<CellId>,
        min: f64,
        max: f64,
        step: f64,
    },
    Select {
        style: IrExpr,
        options: Vec<(String, String)>,
        selected: Option<IrExpr>,
    },
    Svg {
        style: IrExpr,
        children: CellId,
    },
    SvgCircle {
        cx: IrExpr,
        cy: IrExpr,
        r: IrExpr,
        style: IrExpr,
    },
}

// ---------------------------------------------------------------------------
// Program
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct IrProgram {
    pub cells: Vec<CellInfo>,
    pub events: Vec<EventInfo>,
    pub nodes: Vec<IrNode>,
    pub document: Option<CellId>,
    pub render_surface: Option<RenderSurface>,
    pub functions: Vec<IrFunction>,
    /// Tag string → encoded f64 value mapping.
    /// Tags are encoded as positive f64 values starting at 1.0.
    pub tag_table: Vec<String>,
    /// Map from parent CellId → field sub-cells for object-typed cells.
    /// Used by codegen to resolve text from namespace cells (e.g., finding the
    /// "title" field of a todo item for list display).
    pub cell_field_cells: HashMap<CellId, HashMap<String, CellId>>,
}

impl IrProgram {
    #[must_use]
    pub fn render_surface(&self) -> RenderSurface {
        self.render_surface.unwrap_or(RenderSurface::Document)
    }

    #[must_use]
    pub fn render_root(&self) -> Option<RenderRootHandle<CellId>> {
        self.nodes.iter().find_map(|node| {
            if let IrNode::Document {
                kind,
                root,
                lights,
                geometry,
            } = node
            {
                Some(if kind.is_scene() {
                    RenderRootHandle::scene(*root, *lights, *geometry)
                } else {
                    RenderRootHandle::new(*kind, *root)
                })
            } else {
                None
            }
        })
    }
}

#[derive(Debug)]
pub struct CellInfo {
    pub name: String,
    pub span: Span,
}

#[derive(Debug)]
pub struct EventInfo {
    pub name: String,
    pub source: EventSource,
    pub span: Span,
    /// Cells that are set by the host before this event fires.
    /// The codegen emits downstream updates for these in the event handler.
    pub payload_cells: Vec<CellId>,
}

#[derive(Debug)]
pub enum EventSource {
    Link { element: CellId, event_name: String },
    Timer,
    Router,
    Synthetic,
}

#[derive(Debug)]
pub struct IrFunction {
    pub name: String,
    pub params: Vec<String>,
    pub param_cells: Vec<CellId>,
    pub body: IrExpr,
}

/// Short debug representation of an IrNode (for logging).
pub fn node_debug_short(node: &IrNode) -> String {
    match node {
        IrNode::Derived { cell, expr } => {
            format!("Derived(cell={}, expr={:?})", cell.0, expr_short(expr))
        }
        IrNode::Hold {
            cell,
            trigger_bodies,
            ..
        } => format!(
            "Hold(cell={}, triggers={:?})",
            cell.0,
            trigger_bodies.iter().map(|(e, _)| e).collect::<Vec<_>>()
        ),
        IrNode::Latest { target, arms } => {
            format!("Latest(target={}, arms={})", target.0, arms.len())
        }
        IrNode::Then {
            cell,
            trigger,
            body,
        } => format!(
            "Then(cell={}, trigger={:?}, body={})",
            cell.0,
            trigger,
            expr_short(body)
        ),
        IrNode::When { cell, source, arms } => format!(
            "When(cell={}, source={}, arms={})",
            cell.0,
            source.0,
            arms.len()
        ),
        IrNode::While {
            cell,
            source,
            deps,
            arms,
        } => format!(
            "While(cell={}, source={}, deps={:?}, arms={})",
            cell.0,
            source.0,
            deps.iter().map(|d| d.0).collect::<Vec<_>>(),
            arms.len()
        ),
        IrNode::Timer { event, .. } => format!("Timer(event={:?})", event),
        IrNode::Element { cell, kind, .. } => {
            format!("Element(cell={}, kind={})", cell.0, element_kind_name(kind))
        }
        IrNode::Document {
            kind,
            root,
            lights,
            geometry,
        } => format!(
            "{:?}(root={}, lights={:?}, geometry={:?})",
            kind, root.0, lights, geometry
        ),
        IrNode::TextInterpolation { cell, parts } => {
            format!("TextInterp(cell={}, parts={})", cell.0, parts.len())
        }
        IrNode::MathSum { cell, input } => format!("MathSum(cell={}, input={})", cell.0, input.0),
        IrNode::PipeThrough { cell, source } => {
            format!("PipeThrough(cell={}, source={})", cell.0, source.0)
        }
        IrNode::StreamSkip {
            cell,
            source,
            count,
            seen_cell,
        } => {
            format!(
                "StreamSkip(cell={}, source={}, count={}, seen={})",
                cell.0, source.0, count, seen_cell.0
            )
        }
        IrNode::CustomCall { cell, path, .. } => {
            format!("CustomCall(cell={}, path={:?})", cell.0, path)
        }
        IrNode::ListAppend {
            cell,
            source,
            item,
            trigger,
            watch_cell,
        } => format!(
            "ListAppend(cell={}, source={}, item={}, trigger={:?}, watch={:?})",
            cell.0,
            source.0,
            item.0,
            trigger,
            watch_cell.map(|c| c.0)
        ),
        IrNode::ListClear {
            cell,
            source,
            trigger,
        } => format!(
            "ListClear(cell={}, source={}, trigger={:?})",
            cell.0, source.0, trigger
        ),
        IrNode::ListCount { cell, source } => {
            format!("ListCount(cell={}, source={})", cell.0, source.0)
        }
        IrNode::ListMap {
            cell,
            source,
            item_name,
            template_cell_range,
            template_event_range,
            ..
        } => format!(
            "ListMap(cell={}, source={}, item={}, cells={:?}, events={:?})",
            cell.0, source.0, item_name, template_cell_range, template_event_range
        ),
        IrNode::ListRemove {
            cell,
            source,
            trigger,
            predicate,
            item_cell,
            item_field_cells,
        } => format!(
            "ListRemove(cell={}, source={}, trigger={:?}, pred={:?}, item={:?}, fields={:?})",
            cell.0,
            source.0,
            trigger,
            predicate.map(|p| p.0),
            item_cell.map(|c| c.0),
            item_field_cells
                .iter()
                .map(|(n, c)| format!("{}:{}", n, c.0))
                .collect::<Vec<_>>()
        ),
        IrNode::ListRetain {
            cell,
            source,
            predicate,
            item_cell,
            item_field_cells,
        } => format!(
            "ListRetain(cell={}, source={}, pred={:?}, item={:?}, fields={:?})",
            cell.0,
            source.0,
            predicate.map(|p| p.0),
            item_cell.map(|c| c.0),
            item_field_cells
                .iter()
                .map(|(n, c)| format!("{}:{}", n, c.0))
                .collect::<Vec<_>>()
        ),
        IrNode::ListEvery {
            cell,
            source,
            predicate,
            item_cell,
            item_field_cells,
        } => format!(
            "ListEvery(cell={}, source={}, pred={:?}, item={:?}, fields={:?})",
            cell.0,
            source.0,
            predicate.map(|p| p.0),
            item_cell.map(|c| c.0),
            item_field_cells
                .iter()
                .map(|(n, c)| format!("{}:{}", n, c.0))
                .collect::<Vec<_>>()
        ),
        IrNode::ListAny {
            cell,
            source,
            predicate,
            item_cell,
            item_field_cells,
        } => format!(
            "ListAny(cell={}, source={}, pred={:?}, item={:?}, fields={:?})",
            cell.0,
            source.0,
            predicate.map(|p| p.0),
            item_cell.map(|c| c.0),
            item_field_cells
                .iter()
                .map(|(n, c)| format!("{}:{}", n, c.0))
                .collect::<Vec<_>>()
        ),
        IrNode::ListIsEmpty { cell, source } => {
            format!("ListIsEmpty(cell={}, source={})", cell.0, source.0)
        }
        IrNode::RouterGoTo { cell, source } => {
            format!("RouterGoTo(cell={}, source={})", cell.0, source.0)
        }
        IrNode::TextTrim { cell, source } => {
            format!("TextTrim(cell={}, source={})", cell.0, source.0)
        }
        IrNode::TextIsNotEmpty { cell, source } => {
            format!("TextIsNotEmpty(cell={}, source={})", cell.0, source.0)
        }
        IrNode::TextToNumber { cell, source, .. } => {
            format!("TextToNumber(cell={}, source={})", cell.0, source.0)
        }
        IrNode::TextStartsWith { cell, source, prefix } => {
            format!("TextStartsWith(cell={}, source={}, prefix={})", cell.0, source.0, prefix.0)
        }
        IrNode::MathRound { cell, source } => {
            format!("MathRound(cell={}, source={})", cell.0, source.0)
        }
        IrNode::MathMin { cell, source, b } => {
            format!("MathMin(cell={}, source={}, b={})", cell.0, source.0, b.0)
        }
        IrNode::MathMax { cell, source, b } => {
            format!("MathMax(cell={}, source={}, b={})", cell.0, source.0, b.0)
        }
        IrNode::HoldLoop {
            cell, field_cells, ..
        } => format!(
            "HoldLoop(cell={}, fields={:?})",
            cell.0,
            field_cells
                .iter()
                .map(|(n, c)| format!("{}:{}", n, c.0))
                .collect::<Vec<_>>()
        ),
    }
}

pub fn expr_short(expr: &IrExpr) -> String {
    match expr {
        IrExpr::Constant(v) => format!("Const({:?})", v),
        IrExpr::CellRead(c) => format!("Cell({})", c.0),
        IrExpr::BinOp { op, .. } => format!("BinOp({:?})", op),
        IrExpr::TextConcat(segs) => format!("TextConcat({}segs)", segs.len()),
        _ => format!("{:?}", std::mem::discriminant(expr)),
    }
}

fn element_kind_name(kind: &ElementKind) -> &'static str {
    match kind {
        ElementKind::Button { .. } => "Button",
        ElementKind::TextInput { .. } => "TextInput",
        ElementKind::Checkbox { .. } => "Checkbox",
        ElementKind::Stripe { .. } => "Stripe",
        ElementKind::Container { .. } => "Container",
        ElementKind::Label { .. } => "Label",
        ElementKind::Stack { .. } => "Stack",
        ElementKind::Link { .. } => "Link",
        ElementKind::Paragraph { .. } => "Paragraph",
        ElementKind::Block { .. } => "Block",
        ElementKind::Text { .. } => "Text",
        ElementKind::Slider { .. } => "Slider",
        ElementKind::Select { .. } => "Select",
        ElementKind::Svg { .. } => "Svg",
        ElementKind::SvgCircle { .. } => "SvgCircle",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{CellId, IrNode, IrProgram};
    use boon_scene::RenderSurface;

    #[test]
    fn render_root_preserves_scene_handles() {
        let program = IrProgram {
            cells: Vec::new(),
            events: Vec::new(),
            nodes: vec![IrNode::Document {
                kind: RenderSurface::Scene,
                root: CellId(1),
                lights: Some(CellId(2)),
                geometry: Some(CellId(3)),
            }],
            document: Some(CellId(1)),
            render_surface: Some(RenderSurface::Scene),
            functions: Vec::new(),
            tag_table: Vec::new(),
            cell_field_cells: HashMap::new(),
        };

        let render_root = program.render_root().expect("render root should exist");
        assert!(render_root.is_scene());
        assert_eq!(render_root.root.0, 1);
        let scene = render_root.scene.expect("scene metadata should exist");
        assert_eq!(scene.lights.expect("lights cell").0, 2);
        assert_eq!(scene.geometry.expect("geometry cell").0, 3);
    }
}
