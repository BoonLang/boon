//! AST → IR lowering for the WASM compilation engine.
//!
//! Two-pass approach:
//! 1. Registration: Walk top-level expressions, collect variable and function definitions.
//! 2. Lowering: Walk each variable's expression tree, emit IrNodes, allocate CellIds/EventIds.

use std::collections::{HashMap, HashSet};
use std::env;

use boon_scene::RenderSurface;

use crate::parser::Span;
use crate::parser::static_expression::{
    Alias, Argument, ArithmeticOperator, Comparator, Expression, Literal, Spanned, TextPart,
    Variable,
};

/// Stored function definition for inlining at call sites.
#[derive(Clone)]
struct FuncDef {
    params: Vec<String>,
    body: Spanned<Expression>,
}

/// External function definition for multi-file support.
/// (qualified_name, params, body, module_name)
pub(super) type ExternalFunction = (String, Vec<String>, Spanned<Expression>, Option<String>);

use super::ir::*;

// ---------------------------------------------------------------------------
// Saved element bindings (for proper nesting of Element calls)
// ---------------------------------------------------------------------------

/// Saved state of element.* bindings from `name_to_cell` and `element_events`.
/// Used by `process_element_self_ref` / `restore_element_self_ref` to properly
/// restore outer element bindings when nested Element calls finish.
#[derive(Default)]
struct SavedElementBindings {
    bindings: Vec<(String, Option<CellId>)>,
    events: Option<HashMap<String, EventId>>,
    /// The hovered_cell allocated by this element's own `element: [hovered: LINK]`.
    /// None if the element didn't declare its own `element:` argument.
    /// This prevents nested elements from inheriting the parent's hover handler,
    /// which would cause mouseleave on the child to incorrectly reset the parent's
    /// hovered state.
    hovered_cell: Option<CellId>,
    /// The `.text` property cell allocated for this element self-ref, when present.
    text_cell: Option<CellId>,
}

// ---------------------------------------------------------------------------
// Compile errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub(super) struct CompileError {
    pub(super) span: Span,
    pub(super) message: String,
}

// ---------------------------------------------------------------------------
// Lowering context
// ---------------------------------------------------------------------------

struct Lowerer {
    cells: Vec<CellInfo>,
    events: Vec<EventInfo>,
    nodes: Vec<IrNode>,
    functions: Vec<IrFunction>,
    document: Option<CellId>,
    render_surface: Option<RenderSurface>,
    errors: Vec<CompileError>,

    /// Map from variable name → CellId for name resolution.
    name_to_cell: HashMap<String, CellId>,

    /// Map from function name → FuncId.
    name_to_func: HashMap<String, FuncId>,

    /// Map from (element_variable_name, event_name) → EventId.
    /// Pre-allocated in Pass 1 by scanning element definitions for LINK patterns.
    element_events: HashMap<String, HashMap<String, EventId>>,

    /// Map from CellId → EventId for cells that represent event sources.
    /// Populated when creating Timer nodes or PipeThrough nodes from event sources.
    cell_events: HashMap<CellId, EventId>,

    /// Tag encoding: tag name → index in tag_table.
    tag_to_index: HashMap<String, usize>,
    tag_table: Vec<String>,

    /// Function definitions stored for inlining at call sites.
    /// Key: function name, Value: (params, body AST).
    func_defs: HashMap<String, FuncDef>,

    /// Module name for each qualified function (e.g., "Theme/get" → "Theme").
    /// Used for intra-module resolution.
    function_modules: HashMap<String, String>,

    /// Current module context during function body inlining.
    current_module: Option<String>,

    /// Current outer variable name being lowered. Used to propagate LINK events
    /// from the outer variable to elements created inside function bodies.
    current_var_name: Option<String>,

    /// Current PASSED context expression. Set when a function is called with
    /// `PASS: [...]`. Used to resolve `PASSED.x.y.z` paths.
    current_passed: Option<Spanned<Expression>>,

    /// Recursion depth guard for resolve_passed_path ↔ lower_alias cycles.
    passed_resolve_depth: usize,

    /// Function inlining depth guard.
    inline_depth: usize,

    /// Active user-defined call stack for recursion detection.
    active_function_calls: Vec<String>,

    /// Map from CellId → field sub-cells for object-typed HOLD cells.
    /// Propagated through PipeThrough/CustomCall chains so field access
    /// (e.g., `.current`) resolves to the correct sub-cell.
    cell_field_cells: HashMap<CellId, HashMap<String, CellId>>,

    /// Map from list CellId → constructor function name.
    /// When a LIST is constructed with function call items (e.g., `LIST { new_todo(...) }`),
    /// the constructor function name is recorded. Propagated through list operations
    /// (append, remove, retain) so that List/map can re-inline the constructor in the
    /// template range for per-item cell access.
    list_item_constructor: HashMap<CellId, String>,

    /// Compile-time representative field expressions for list items.
    /// Used to preserve object/list shape through List/map template lowering
    /// without rediscovering it at runtime.
    list_item_field_exprs: HashMap<CellId, HashMap<String, IrExpr>>,

    /// Pending constructor name from the most recent LIST expression lowering.
    /// Consumed by `lower_expr_to_cell` when the ListConstruct gets assigned to a cell.
    pending_list_constructor: Option<String>,

    /// When true, object literals are always lowered as stores (creating cells for each field)
    /// even if the fields aren't reactive. Set during list constructor template inlining
    /// so that field access resolves to actual cells instead of ObjectConstruct.
    force_object_store: bool,

    /// When true, preserve user-defined function calls as runtime IR instead of
    /// aggressively inlining them during expression lowering.
    preserve_runtime_function_calls: bool,

    /// Cells known to hold constant values at compile time.
    /// Used for constant folding of WHEN/WHILE arms during function inlining:
    /// when a cell is known to be e.g. `Tag("Material")`, only the matching arm
    /// needs to be lowered, eliminating dead arms and dramatically reducing IR size
    /// for multi-file programs like todo_mvc_physical (90 Theme calls × 4 themes × 11 categories).
    constant_cells: HashMap<CellId, IrValue>,

    /// Cached per-function decision for whether inlining the function body
    /// requires a full name_to_cell snapshot instead of prefix-scoped restore.
    function_requires_full_name_snapshot: HashMap<String, bool>,

    /// Cached per-function decision for whether param-prefixed names such as
    /// `cell.cell_elements.display` must be preserved across inlining.
    function_requires_prefixed_name_snapshot: HashMap<String, bool>,

    /// Deferred per-item List/remove operations. When List/remove(item, on: item.X.Y.event.Z)
    /// can't find the per-item event (because it's created later in List/map template lowering),
    /// the remove is deferred and resolved during List/map where the events exist.
    /// Key: list source CellId, Value: list of pending removes.
    pending_per_item_removes: HashMap<CellId, Vec<PendingPerItemRemove>>,
}

/// A deferred per-item List/remove operation.
/// Created when List/remove references per-item events that don't exist yet
/// (they'll be created during List/map template lowering).
#[derive(Clone)]
struct PendingPerItemRemove {
    /// The item parameter name used in the List/remove declaration (e.g., "item").
    item_name: String,
    /// Index of the ListRemove node in self.nodes (for patching the trigger).
    node_index: usize,
    /// The original `on:` expression AST (e.g., `item.todo_elements.remove_todo_button.event.press`).
    on_expr: Spanned<Expression>,
}

impl Lowerer {
    fn new() -> Self {
        Self {
            cells: Vec::new(),
            events: Vec::new(),
            nodes: Vec::new(),
            functions: Vec::new(),
            document: None,
            render_surface: None,
            errors: Vec::new(),
            name_to_cell: HashMap::new(),
            name_to_func: HashMap::new(),
            element_events: HashMap::new(),
            cell_events: HashMap::new(),
            tag_to_index: HashMap::new(),
            tag_table: Vec::new(),
            func_defs: HashMap::new(),
            function_modules: HashMap::new(),
            current_module: None,
            cell_field_cells: HashMap::new(),
            list_item_constructor: HashMap::new(),
            list_item_field_exprs: HashMap::new(),
            pending_list_constructor: None,
            current_var_name: None,
            current_passed: None,
            passed_resolve_depth: 0,
            inline_depth: 0,
            active_function_calls: Vec::new(),
            force_object_store: false,
            preserve_runtime_function_calls: false,
            constant_cells: HashMap::new(),
            function_requires_full_name_snapshot: HashMap::new(),
            function_requires_prefixed_name_snapshot: HashMap::new(),
            pending_per_item_removes: HashMap::new(),
        }
    }

    /// Register external functions from parsed module files.
    fn register_external_functions(&mut self, ext_fns: &[ExternalFunction]) {
        for (qualified_name, params, body, module_name) in ext_fns {
            let func_id = FuncId(u32::try_from(self.name_to_func.len()).unwrap());
            self.name_to_func.insert(qualified_name.clone(), func_id);
            self.func_defs.insert(
                qualified_name.clone(),
                FuncDef {
                    params: params.clone(),
                    body: body.clone(),
                },
            );
            if let Some(module) = module_name {
                self.function_modules
                    .insert(qualified_name.clone(), module.clone());
            }
        }
    }

    fn expr_requires_full_name_snapshot(&self, expr: &Expression) -> bool {
        match expr {
            Expression::Block { .. }
            | Expression::Variable(_)
            | Expression::Function { .. }
            | Expression::Hold { .. } => true,
            Expression::FunctionCall { path, arguments } => {
                let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                let resolved_fn = if path_strs.len() == 1 {
                    self.resolve_func_name(path_strs[0])
                } else if path_strs.len() == 2 {
                    let qualified = format!("{}/{}", path_strs[0], path_strs[1]);
                    self.resolve_func_name(&qualified)
                } else {
                    None
                };
                if resolved_fn.is_some() {
                    return true;
                }
                arguments.iter().any(|arg| {
                    arg.node
                        .value
                        .as_ref()
                        .is_some_and(|value| self.expr_requires_full_name_snapshot(&value.node))
                })
            }
            Expression::Pipe { from, to } => {
                self.expr_requires_full_name_snapshot(&from.node)
                    || self.expr_requires_full_name_snapshot(&to.node)
            }
            Expression::Latest { inputs } | Expression::List { items: inputs } => inputs
                .iter()
                .any(|input| self.expr_requires_full_name_snapshot(&input.node)),
            Expression::Object(object) => object
                .variables
                .iter()
                .any(|field| self.expr_requires_full_name_snapshot(&field.node.value.node)),
            Expression::TaggedObject { object, .. } => object
                .variables
                .iter()
                .any(|field| self.expr_requires_full_name_snapshot(&field.node.value.node)),
            Expression::Then { body } | Expression::Flush { value: body } => {
                self.expr_requires_full_name_snapshot(&body.node)
            }
            Expression::When { arms } | Expression::While { arms } => arms
                .iter()
                .any(|arm| self.expr_requires_full_name_snapshot(&arm.body.node)),
            Expression::PostfixFieldAccess { expr, .. } | Expression::Spread { value: expr } => {
                self.expr_requires_full_name_snapshot(&expr.node)
            }
            Expression::Comparator(comparator) => match comparator {
                Comparator::Equal {
                    operand_a,
                    operand_b,
                }
                | Comparator::NotEqual {
                    operand_a,
                    operand_b,
                }
                | Comparator::Greater {
                    operand_a,
                    operand_b,
                }
                | Comparator::GreaterOrEqual {
                    operand_a,
                    operand_b,
                }
                | Comparator::Less {
                    operand_a,
                    operand_b,
                }
                | Comparator::LessOrEqual {
                    operand_a,
                    operand_b,
                } => {
                    self.expr_requires_full_name_snapshot(&operand_a.node)
                        || self.expr_requires_full_name_snapshot(&operand_b.node)
                }
            },
            Expression::ArithmeticOperator(operator) => match operator {
                ArithmeticOperator::Negate { operand } => {
                    self.expr_requires_full_name_snapshot(&operand.node)
                }
                ArithmeticOperator::Add {
                    operand_a,
                    operand_b,
                }
                | ArithmeticOperator::Subtract {
                    operand_a,
                    operand_b,
                }
                | ArithmeticOperator::Multiply {
                    operand_a,
                    operand_b,
                }
                | ArithmeticOperator::Divide {
                    operand_a,
                    operand_b,
                } => {
                    self.expr_requires_full_name_snapshot(&operand_a.node)
                        || self.expr_requires_full_name_snapshot(&operand_b.node)
                }
            },
            Expression::Bits { size } => self.expr_requires_full_name_snapshot(&size.node),
            Expression::Memory { address } => self.expr_requires_full_name_snapshot(&address.node),
            Expression::Bytes { data } => data
                .iter()
                .any(|item| self.expr_requires_full_name_snapshot(&item.node)),
            Expression::Alias(_)
            | Expression::Literal(_)
            | Expression::Map { .. }
            | Expression::Link
            | Expression::LinkSetter { .. }
            | Expression::FieldAccess { .. }
            | Expression::Skip
            | Expression::TextLiteral { .. } => false,
        }
    }

    fn function_requires_full_name_snapshot(&mut self, fn_name: &str, func_def: &FuncDef) -> bool {
        if let Some(&cached) = self.function_requires_full_name_snapshot.get(fn_name) {
            return cached;
        }
        let requires = self.expr_requires_full_name_snapshot(&func_def.body.node);
        self.function_requires_full_name_snapshot
            .insert(fn_name.to_string(), requires);
        requires
    }

    fn function_requires_prefixed_name_snapshot(
        &mut self,
        fn_name: &str,
        func_def: &FuncDef,
    ) -> bool {
        if let Some(&cached) = self.function_requires_prefixed_name_snapshot.get(fn_name) {
            return cached;
        }
        let params_set: std::collections::HashSet<String> =
            func_def.params.iter().cloned().collect();
        let requires = Self::expr_uses_prefixed_param_paths(&func_def.body.node, &params_set);
        self.function_requires_prefixed_name_snapshot
            .insert(fn_name.to_string(), requires);
        requires
    }

    fn capture_prefixed_name_bindings(&self, prefixes: &[String]) -> HashMap<String, CellId> {
        self.name_to_cell
            .iter()
            .filter_map(|(name, &cell)| {
                prefixes
                    .iter()
                    .any(|prefix| name == prefix || name.starts_with(&format!("{prefix}.")))
                    .then_some((name.clone(), cell))
            })
            .collect()
    }

    fn capture_exact_name_bindings(&self, names: &[String]) -> Vec<(String, Option<CellId>)> {
        names
            .iter()
            .map(|name| (name.clone(), self.name_to_cell.get(name).copied()))
            .collect()
    }

    fn restore_prefixed_name_bindings(
        &mut self,
        prefixes: &[String],
        saved: HashMap<String, CellId>,
    ) {
        self.name_to_cell.retain(|name, _| {
            !prefixes
                .iter()
                .any(|prefix| name == prefix || name.starts_with(&format!("{prefix}.")))
        });
        self.name_to_cell.extend(saved);
    }

    fn restore_exact_name_bindings(&mut self, saved: Vec<(String, Option<CellId>)>) {
        for (name, cell) in saved {
            if let Some(cell) = cell {
                self.name_to_cell.insert(name, cell);
            } else {
                self.name_to_cell.remove(&name);
            }
        }
    }

    fn expr_uses_prefixed_param_paths(
        expr: &Expression,
        params: &std::collections::HashSet<String>,
    ) -> bool {
        match expr {
            Expression::Alias(Alias::WithoutPassed { parts, .. }) => {
                parts.len() > 1 && params.contains(parts[0].as_str())
            }
            Expression::FieldAccess { path } => path.len() > 1 && params.contains(path[0].as_str()),
            Expression::PostfixFieldAccess { expr, .. } => {
                Self::expr_uses_prefixed_param_paths(&expr.node, params)
            }
            Expression::Pipe { from, to } => {
                Self::expr_uses_prefixed_param_paths(&from.node, params)
                    || Self::expr_uses_prefixed_param_paths(&to.node, params)
            }
            Expression::FunctionCall { arguments, .. } => arguments.iter().any(|arg| {
                arg.node
                    .value
                    .as_ref()
                    .is_some_and(|value| Self::expr_uses_prefixed_param_paths(&value.node, params))
            }),
            Expression::Object(object) | Expression::TaggedObject { object, .. } => object
                .variables
                .iter()
                .any(|field| Self::expr_uses_prefixed_param_paths(&field.node.value.node, params)),
            Expression::Block { variables, output } => {
                variables
                    .iter()
                    .any(|var| Self::expr_uses_prefixed_param_paths(&var.node.value.node, params))
                    || Self::expr_uses_prefixed_param_paths(&output.node, params)
            }
            Expression::Hold { body, .. } => {
                Self::expr_uses_prefixed_param_paths(&body.node, params)
            }
            Expression::Latest { inputs } => inputs
                .iter()
                .any(|input| Self::expr_uses_prefixed_param_paths(&input.node, params)),
            Expression::Then { body } => Self::expr_uses_prefixed_param_paths(&body.node, params),
            Expression::When { arms } | Expression::While { arms } => arms
                .iter()
                .any(|arm| Self::expr_uses_prefixed_param_paths(&arm.body.node, params)),
            Expression::TextLiteral { parts, .. } => parts.iter().any(|part| match part {
                TextPart::Interpolation { var, .. } => {
                    let pieces: Vec<_> = var.as_str().split('.').collect();
                    pieces.len() > 1 && params.contains(pieces[0])
                }
                TextPart::Text(_) => false,
            }),
            Expression::List { items } => items
                .iter()
                .any(|item| Self::expr_uses_prefixed_param_paths(&item.node, params)),
            Expression::Comparator(comparator) => match comparator {
                Comparator::Equal {
                    operand_a,
                    operand_b,
                }
                | Comparator::NotEqual {
                    operand_a,
                    operand_b,
                }
                | Comparator::Less {
                    operand_a,
                    operand_b,
                }
                | Comparator::LessOrEqual {
                    operand_a,
                    operand_b,
                }
                | Comparator::Greater {
                    operand_a,
                    operand_b,
                }
                | Comparator::GreaterOrEqual {
                    operand_a,
                    operand_b,
                } => {
                    Self::expr_uses_prefixed_param_paths(&operand_a.node, params)
                        || Self::expr_uses_prefixed_param_paths(&operand_b.node, params)
                }
            },
            Expression::ArithmeticOperator(operator) => match operator {
                ArithmeticOperator::Negate { operand } => {
                    Self::expr_uses_prefixed_param_paths(&operand.node, params)
                }
                ArithmeticOperator::Add {
                    operand_a,
                    operand_b,
                }
                | ArithmeticOperator::Subtract {
                    operand_a,
                    operand_b,
                }
                | ArithmeticOperator::Multiply {
                    operand_a,
                    operand_b,
                }
                | ArithmeticOperator::Divide {
                    operand_a,
                    operand_b,
                } => {
                    Self::expr_uses_prefixed_param_paths(&operand_a.node, params)
                        || Self::expr_uses_prefixed_param_paths(&operand_b.node, params)
                }
            },
            Expression::Bits { size } => Self::expr_uses_prefixed_param_paths(&size.node, params),
            Expression::Memory { address } => {
                Self::expr_uses_prefixed_param_paths(&address.node, params)
            }
            Expression::Bytes { data } => data
                .iter()
                .any(|item| Self::expr_uses_prefixed_param_paths(&item.node, params)),
            Expression::Map { entries } => entries
                .iter()
                .any(|entry| Self::expr_uses_prefixed_param_paths(&entry.value.node, params)),
            Expression::Alias(_)
            | Expression::Literal(_)
            | Expression::Link
            | Expression::LinkSetter { .. }
            | Expression::Skip
            | Expression::Variable(_)
            | Expression::Function { .. }
            | Expression::Flush { .. }
            | Expression::Spread { .. } => false,
        }
    }

    /// Look up a function definition by name, with intra-module fallback.
    fn find_func_def(&self, name: &str) -> Option<FuncDef> {
        // Try exact match
        if let Some(f) = self.func_defs.get(name) {
            return Some(f.clone());
        }
        // Intra-module resolution: try current_module/name
        if let Some(module) = &self.current_module {
            let qualified = format!("{}/{}", module, name);
            if let Some(f) = self.func_defs.get(&qualified) {
                return Some(f.clone());
            }
        }
        None
    }

    fn expr_may_carry_namespace_shape(expr: &Expression) -> bool {
        matches!(
            expr,
            Expression::Alias(_)
                | Expression::Pipe { .. }
                | Expression::FunctionCall { .. }
                | Expression::Object(_)
                | Expression::TaggedObject { .. }
                | Expression::List { .. }
                | Expression::Block { .. }
                | Expression::Hold { .. }
                | Expression::Latest { .. }
                | Expression::Then { .. }
                | Expression::When { .. }
                | Expression::While { .. }
                | Expression::FieldAccess { .. }
        )
    }

    /// Resolve a function name to its qualified name (for module context tracking).
    fn resolve_func_name(&self, name: &str) -> Option<String> {
        if self.func_defs.contains_key(name) {
            return Some(name.to_string());
        }
        if let Some(module) = &self.current_module {
            let qualified = format!("{}/{}", module, name);
            if self.func_defs.contains_key(&qualified) {
                return Some(qualified);
            }
        }
        None
    }

    fn alloc_cell(&mut self, name: &str, span: Span) -> CellId {
        let id = CellId(u32::try_from(self.cells.len()).unwrap());
        self.cells.push(CellInfo {
            name: name.to_string(),
            span,
        });
        id
    }

    fn alloc_event(&mut self, name: &str, source: EventSource, span: Span) -> EventId {
        let id = EventId(u32::try_from(self.events.len()).unwrap());
        self.events.push(EventInfo {
            name: name.to_string(),
            source,
            span,
            payload_cells: Vec::new(),
        });
        id
    }

    fn error(&mut self, span: Span, message: impl Into<String>) {
        self.errors.push(CompileError {
            span,
            message: message.into(),
        });
    }

    /// Intern a tag string and return its encoded f64 value.
    /// Tags are encoded as (index + 1) to distinguish from 0.0 (void/unset).
    fn intern_tag(&mut self, tag: &str) -> f64 {
        if let Some(&idx) = self.tag_to_index.get(tag) {
            (idx + 1) as f64
        } else {
            let idx = self.tag_table.len();
            self.tag_table.push(tag.to_string());
            self.tag_to_index.insert(tag.to_string(), idx);
            (idx + 1) as f64
        }
    }

    /// Propagate list_item_constructor from a source list cell to a derived target cell.
    /// Called after List/append, List/remove, List/retain, List/clear to maintain
    /// the constructor chain so List/map can re-inline it.
    /// Extract the constructor function name from an expression.
    /// Walks pipe chains to find the rightmost user-defined function call.
    /// e.g., `title_to_save |> new_todo()` → Some("new_todo")
    fn extract_constructor_from_expr(&self, expr: &Expression) -> Option<String> {
        match expr {
            Expression::FunctionCall { path, .. } => {
                let name = path.last()?.as_str().to_string();
                if self.func_defs.contains_key(&name) {
                    Some(name)
                } else {
                    None
                }
            }
            Expression::Pipe { to, .. } => self.extract_constructor_from_expr(&to.node),
            _ => None,
        }
    }

    fn extract_list_constructor_from_value_expr(&self, expr: &Expression) -> Option<String> {
        self.extract_list_constructor_from_value_expr_inner(expr, &mut HashSet::new())
    }

    fn extract_list_constructor_from_value_expr_inner(
        &self,
        expr: &Expression,
        visiting_functions: &mut HashSet<String>,
    ) -> Option<String> {
        match expr {
            Expression::List { items } => items
                .iter()
                .find_map(|item| self.extract_constructor_from_expr(&item.node)),
            Expression::FunctionCall { path, .. } => {
                let resolved_fn = match path
                    .iter()
                    .map(|part| part.as_str())
                    .collect::<Vec<_>>()
                    .as_slice()
                {
                    [name] => self.resolve_func_name(name),
                    [module, name] => self.resolve_func_name(&format!("{module}/{name}")),
                    _ => None,
                }?;
                if !visiting_functions.insert(resolved_fn.clone()) {
                    return None;
                }
                let result = self.func_defs.get(&resolved_fn).and_then(|func_def| {
                    self.extract_list_constructor_from_value_expr_inner(
                        &func_def.body.node,
                        visiting_functions,
                    )
                });
                visiting_functions.remove(&resolved_fn);
                result
            }
            Expression::Pipe { from, to } => {
                if let Expression::FunctionCall { path, arguments } = &to.node {
                    let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                    if path_strs.as_slice() == ["List", "append"] {
                        if let Some(arg) = arguments.iter().find(|a| a.node.name.as_str() == "item")
                        {
                            if let Some(value) = &arg.node.value {
                                if let Some(name) = self.extract_constructor_from_expr(&value.node)
                                {
                                    return Some(name);
                                }
                            }
                        }
                    }
                }
                self.extract_list_constructor_from_value_expr_inner(&from.node, visiting_functions)
            }
            Expression::Block { output, .. } => self
                .extract_list_constructor_from_value_expr_inner(&output.node, visiting_functions),
            _ => None,
        }
    }

    /// Find the list item constructor for a cell by following CellRead/Derived chains.
    /// This handles cases where PASSED or alias resolution creates intermediate cells
    /// that don't have list_item_constructor propagated directly.
    fn find_list_constructor(&self, cell: CellId) -> Option<String> {
        let mut current = cell;
        for _ in 0..20 {
            if let Some(name) = self.list_item_constructor.get(&current) {
                return Some(name.clone());
            }
            // Follow through Derived(CellRead) or PipeThrough chains.
            let mut found_source = None;
            for node in self.nodes.iter().rev() {
                match node {
                    IrNode::Derived {
                        cell: c,
                        expr: IrExpr::CellRead(src),
                    } if *c == current => {
                        found_source = Some(*src);
                        break;
                    }
                    IrNode::Derived { cell: c, expr } if *c == current => {
                        if let Some(field_cell) = self.resolve_field_access_expr_to_cell(expr) {
                            found_source = Some(field_cell);
                            break;
                        }
                    }
                    IrNode::PipeThrough { cell: c, source } if *c == current => {
                        found_source = Some(*source);
                        break;
                    }
                    IrNode::ListRetain {
                        cell: c, source, ..
                    } if *c == current => {
                        found_source = Some(*source);
                        break;
                    }
                    IrNode::ListRemove {
                        cell: c, source, ..
                    } if *c == current => {
                        found_source = Some(*source);
                        break;
                    }
                    IrNode::ListAppend {
                        cell: c, source, ..
                    } if *c == current => {
                        found_source = Some(*source);
                        break;
                    }
                    IrNode::ListClear {
                        cell: c, source, ..
                    } if *c == current => {
                        found_source = Some(*source);
                        break;
                    }
                    _ => {}
                }
            }
            match found_source {
                Some(src) => current = src,
                None => return None,
            }
        }
        None
    }

    fn propagate_list_constructor(&mut self, source: CellId, target: CellId) {
        if let Some(c) = self.find_list_constructor(source) {
            self.list_item_constructor.insert(target, c);
        }
        if let Some(fields) = self.find_list_item_field_exprs(source) {
            self.list_item_field_exprs.insert(target, fields);
            let span = self.cells[target.0 as usize].span;
            let _ = self.materialize_list_item_field_cells(target, target, span);
        }
        // Also propagate pending per-item removes along the list chain.
        if let Some(removes) = self.pending_per_item_removes.get(&source).cloned() {
            self.pending_per_item_removes
                .entry(target)
                .or_default()
                .extend(removes);
        }
    }

    fn extract_list_item_field_exprs_from_expr(
        &self,
        expr: &IrExpr,
    ) -> Option<HashMap<String, IrExpr>> {
        match expr {
            IrExpr::ListConstruct(items) => items.first().and_then(|item| match item {
                IrExpr::ObjectConstruct(fields) | IrExpr::TaggedObject { fields, .. } => Some(
                    fields
                        .iter()
                        .map(|(name, field_expr)| {
                            (name.clone(), self.reduce_representative_expr(field_expr))
                        })
                        .collect(),
                ),
                IrExpr::CellRead(cell) => {
                    let source_cell = self.find_metadata_source_cell(*cell).unwrap_or(*cell);
                    self.resolve_cell_to_inline_object(source_cell)
                        .map(|fields| {
                            fields
                                .into_iter()
                                .map(|(name, field_expr)| {
                                    let representative_expr =
                                        self.reduce_representative_expr(&field_expr);
                                    (name, representative_expr)
                                })
                                .collect()
                        })
                        .or_else(|| {
                            self.resolve_cell_field_cells(source_cell).map(|fields| {
                                fields
                                    .into_iter()
                                    .map(|(name, field_cell)| {
                                        let representative_cell = self
                                            .find_immediate_source_cell(field_cell)
                                            .map(|source| {
                                                self.canonicalize_representative_cell(source)
                                            })
                                            .unwrap_or_else(|| {
                                                self.canonicalize_representative_cell(field_cell)
                                            });
                                        (name, IrExpr::CellRead(representative_cell))
                                    })
                                    .collect()
                            })
                        })
                        .or_else(|| {
                            self.nodes.iter().rev().find_map(|node| match node {
                                IrNode::Derived {
                                    cell: source_cell,
                                    expr: IrExpr::ObjectConstruct(fields),
                                }
                                | IrNode::Derived {
                                    cell: source_cell,
                                    expr: IrExpr::TaggedObject { fields, .. },
                                } if *source_cell == *cell => Some(
                                    fields
                                        .iter()
                                        .map(|(name, field_expr)| {
                                            (
                                                name.clone(),
                                                self.reduce_representative_expr(field_expr),
                                            )
                                        })
                                        .collect(),
                                ),
                                _ => None,
                            })
                        })
                }
                _ => None,
            }),
            _ => None,
        }
    }

    fn extract_item_field_exprs_from_template_expr(
        &self,
        expr: &IrExpr,
    ) -> Option<HashMap<String, IrExpr>> {
        match expr {
            IrExpr::ObjectConstruct(fields) | IrExpr::TaggedObject { fields, .. } => Some(
                fields
                    .iter()
                    .map(|(name, field_expr)| {
                        (name.clone(), self.reduce_representative_expr(field_expr))
                    })
                    .collect(),
            ),
            IrExpr::CellRead(cell) => {
                let source_cell = self.find_metadata_source_cell(*cell).unwrap_or(*cell);
                self.resolve_cell_to_inline_object(source_cell)
                    .map(|fields| {
                        fields
                            .into_iter()
                            .map(|(name, field_expr)| {
                                (name, self.reduce_representative_expr(&field_expr))
                            })
                            .collect()
                    })
                    .or_else(|| {
                        self.resolve_cell_field_cells(source_cell).map(|fields| {
                            fields
                                .into_iter()
                                .map(|(name, field_cell)| {
                                    let representative_cell = self
                                        .find_immediate_source_cell(field_cell)
                                        .map(|source| self.canonicalize_representative_cell(source))
                                        .unwrap_or_else(|| {
                                            self.canonicalize_representative_cell(field_cell)
                                        });
                                    (name, IrExpr::CellRead(representative_cell))
                                })
                                .collect()
                        })
                    })
            }
            _ => self
                .resolve_field_access_expr_to_cell(expr)
                .and_then(|cell| {
                    self.extract_item_field_exprs_from_template_expr(&IrExpr::CellRead(cell))
                }),
        }
    }

    fn representative_list_item_fields(
        &self,
        source_cell: CellId,
    ) -> Option<Vec<(String, IrExpr)>> {
        if let Some(fields) = self.nodes.iter().rev().find_map(|node| match node {
            IrNode::ListMap { cell, template, .. } if *cell == source_cell => {
                match template.as_ref() {
                    IrNode::Derived { cell, expr } => {
                        if let Some(fields) = self.resolve_cell_field_cells(*cell) {
                            let mut fields = fields
                                .into_iter()
                                .map(|(name, field_cell)| {
                                    (
                                        name,
                                        IrExpr::CellRead(self.canonicalize_shape_source_cell(
                                            self.canonicalize_representative_cell(field_cell),
                                        )),
                                    )
                                })
                                .collect::<Vec<_>>();
                            fields.sort_by(|(left, _), (right, _)| left.cmp(right));
                            Some(fields)
                        } else if let Some(fields) = self.resolve_cell_to_inline_object(*cell) {
                            let mut fields = fields
                                .into_iter()
                                .map(|(name, expr)| (name, self.reduce_representative_expr(&expr)))
                                .collect::<Vec<_>>();
                            fields.sort_by(|(left, _), (right, _)| left.cmp(right));
                            Some(fields)
                        } else if let Some(fields) =
                            self.extract_item_field_exprs_from_template_expr(expr)
                        {
                            let mut fields = fields
                                .into_iter()
                                .map(|(name, expr)| (name, self.reduce_representative_expr(&expr)))
                                .collect::<Vec<_>>();
                            fields.sort_by(|(left, _), (right, _)| left.cmp(right));
                            Some(fields)
                        } else {
                            None
                        }
                    }
                    _ => None,
                }
            }
            _ => None,
        }) {
            return Some(fields);
        }

        let representative_item = self.nodes.iter().rev().find_map(|node| match node {
            IrNode::Derived {
                cell,
                expr: IrExpr::ListConstruct(items),
            } if *cell == source_cell => items.first().and_then(|item| match item {
                IrExpr::CellRead(item_cell) => Some(*item_cell),
                _ => None,
            }),
            IrNode::ListMap {
                cell, item_cell, ..
            } if *cell == source_cell => Some(*item_cell),
            _ => None,
        })?;

        if let Some(fields) = self.resolve_cell_field_cells(representative_item) {
            let mut fields = fields
                .into_iter()
                .map(|(name, field_cell)| {
                    (
                        name,
                        IrExpr::CellRead(self.canonicalize_shape_source_cell(
                            self.canonicalize_representative_cell(field_cell),
                        )),
                    )
                })
                .collect::<Vec<_>>();
            fields.sort_by(|(left, _), (right, _)| left.cmp(right));
            return Some(fields);
        }

        self.resolve_cell_to_inline_object(representative_item)
            .map(|fields| {
                let mut fields = fields
                    .into_iter()
                    .map(|(name, expr)| (name, self.reduce_representative_expr(&expr)))
                    .collect::<Vec<_>>();
                fields.sort_by(|(left, _), (right, _)| left.cmp(right));
                fields
            })
            .or_else(|| {
                self.find_list_item_field_exprs(source_cell).map(|fields| {
                    let mut fields = fields
                        .into_iter()
                        .map(|(name, expr)| (name, self.reduce_representative_expr(&expr)))
                        .collect::<Vec<_>>();
                    fields.sort_by(|(left, _), (right, _)| left.cmp(right));
                    fields
                })
            })
    }

    fn reduce_representative_expr(&self, expr: &IrExpr) -> IrExpr {
        self.reduce_representative_expr_inner(expr, &mut HashSet::new())
    }

    fn reduce_representative_expr_inner(
        &self,
        expr: &IrExpr,
        visiting: &mut HashSet<CellId>,
    ) -> IrExpr {
        match expr {
            IrExpr::CellRead(source_cell) => {
                IrExpr::CellRead(self.canonicalize_representative_cell(*source_cell))
            }
            IrExpr::FieldAccess { object, field } => {
                if let Some(object_cell) = self.resolve_field_access_expr_to_cell(object)
                    && visiting.insert(object_cell)
                {
                    if let Some(object_fields) = self.resolve_cell_to_inline_object(object_cell)
                        && let Some((_, nested_expr)) =
                            object_fields.iter().find(|(name, _)| name == field)
                    {
                        let reduced = self.reduce_representative_expr_inner(nested_expr, visiting);
                        visiting.remove(&object_cell);
                        return reduced;
                    }
                    visiting.remove(&object_cell);
                }

                self.resolve_field_access_expr_to_cell(expr)
                    .map(|source_cell| {
                        IrExpr::CellRead(self.canonicalize_representative_cell(source_cell))
                    })
                    .unwrap_or_else(|| expr.clone())
            }
            _ => self
                .resolve_field_access_expr_to_cell(expr)
                .map(|source_cell| {
                    IrExpr::CellRead(self.canonicalize_representative_cell(source_cell))
                })
                .unwrap_or_else(|| expr.clone()),
        }
    }

    fn find_list_item_field_exprs(&self, cell: CellId) -> Option<HashMap<String, IrExpr>> {
        let mut current = cell;
        for _ in 0..20 {
            if let Some(fields) = self.list_item_field_exprs.get(&current) {
                return Some(fields.clone());
            }
            let mut found_source = None;
            for node in self.nodes.iter().rev() {
                match node {
                    IrNode::Derived {
                        cell: c,
                        expr: IrExpr::CellRead(src),
                    } if *c == current => {
                        found_source = Some(*src);
                        break;
                    }
                    IrNode::Derived { cell: c, expr } if *c == current => {
                        if let Some(field_cell) = self.resolve_field_access_expr_to_cell(expr) {
                            found_source = Some(field_cell);
                            break;
                        }
                    }
                    IrNode::PipeThrough { cell: c, source } if *c == current => {
                        found_source = Some(*source);
                        break;
                    }
                    _ => {}
                }
            }
            match found_source {
                Some(src) => current = src,
                None => return None,
            }
        }
        None
    }

    fn canonicalize_representative_cell(&self, cell: CellId) -> CellId {
        let mut current = cell;
        let mut seen = HashSet::new();
        for _ in 0..20 {
            if !seen.insert(current) {
                break;
            }
            let has_concrete_shape = self.resolve_cell_field_cells(current).is_some()
                || self.resolve_cell_to_inline_object(current).is_some();
            if has_concrete_shape {
                let is_list_shape = self.find_list_constructor(current).is_some()
                    || self.find_list_item_field_exprs(current).is_some()
                    || self
                        .find_immediate_source_cell(current)
                        .is_some_and(|source| {
                            self.find_list_constructor(source).is_some()
                                || self.find_list_item_field_exprs(source).is_some()
                        });
                if is_list_shape {
                    if let Some(next) = self.find_metadata_source_cell(current)
                        && next != current
                    {
                        current = next;
                        continue;
                    }
                    if let Some(next) = self.find_immediate_source_cell(current)
                        && next != current
                    {
                        current = next;
                        continue;
                    }
                }
                break;
            }
            if let Some(next) = self.find_immediate_upstream_field_cell(current)
                && next != current
            {
                current = next;
                continue;
            }
            match self.find_metadata_source_cell(current) {
                Some(next) if next != current => current = next,
                _ => break,
            }
        }
        current
    }

    fn find_immediate_upstream_field_cell(&self, cell: CellId) -> Option<CellId> {
        self.nodes.iter().rev().find_map(|node| match node {
            IrNode::Derived {
                cell: c,
                expr: IrExpr::FieldAccess { object, field },
            } if *c == cell => {
                let object_cell = self.resolve_field_access_expr_to_cell(object)?;
                let upstream_object = self.nodes.iter().rev().find_map(|node| match node {
                    IrNode::Derived {
                        cell: source_cell,
                        expr: IrExpr::CellRead(src),
                    } if *source_cell == object_cell => Some(*src),
                    IrNode::PipeThrough {
                        cell: source_cell,
                        source,
                    } if *source_cell == object_cell => Some(*source),
                    _ => None,
                })?;
                self.resolve_cell_field_cells(upstream_object)
                    .and_then(|fields| fields.get(field).copied())
            }
            _ => None,
        })
    }

    fn materialize_list_item_field_cells(
        &mut self,
        target: CellId,
        source: CellId,
        span: Span,
    ) -> bool {
        if self
            .cell_field_cells
            .get(&target)
            .is_some_and(|fields| !fields.is_empty())
        {
            self.list_item_field_exprs.remove(&target);
            return true;
        }
        let has_concrete_item_shape = self.cell_field_cells.contains_key(&source)
            || self.resolve_cell_to_inline_object(source).is_some()
            || self.find_list_item_field_exprs(source).is_some();
        let canonical_source = if has_concrete_item_shape {
            source
        } else {
            self.find_metadata_source_cell(source).unwrap_or(source)
        };
        if canonical_source != source {
            return self.materialize_list_item_field_cells(target, canonical_source, span);
        }
        let source_fields = self.cell_field_cells.get(&source).cloned();
        let source_field_exprs = self.find_list_item_field_exprs(source);
        if source_field_exprs.is_some()
            && let Some(source_fields) = source_fields
        {
            let alias_fields = self.build_field_alias_map(target, &source_fields);
            if !alias_fields.is_empty() {
                self.list_item_field_exprs.remove(&target);
                self.cell_field_cells.insert(target, alias_fields);
                return true;
            }
        }
        let Some(field_exprs) = source_field_exprs else {
            return false;
        };
        let mut inline_fields: Vec<_> = field_exprs
            .iter()
            .map(|(field_name, field_expr)| {
                let inlined =
                    self.inline_cell_reads_in_expr(field_expr.clone(), &mut HashSet::new());
                let prefer_dynamic_item_field = target != source
                    && matches!(
                        &inlined,
                        IrExpr::CellRead(source_cell)
                            if !self.cell_has_concrete_shape(*source_cell)
                                && self.find_list_constructor(*source_cell).is_none()
                                && self.find_list_item_field_exprs(*source_cell).is_none()
                    );
                (
                    field_name.clone(),
                    if prefer_dynamic_item_field {
                        IrExpr::FieldAccess {
                            object: Box::new(IrExpr::CellRead(target)),
                            field: field_name.clone(),
                        }
                    } else {
                        inlined
                    },
                )
            })
            .collect();
        inline_fields.sort_by(|(left, _), (right, _)| left.cmp(right));
        let field_map = self.register_inline_object_field_cells(target, &inline_fields, span);
        if !field_map.is_empty() {
            self.list_item_field_exprs.remove(&target);
            self.cell_field_cells.insert(target, field_map);
            true
        } else {
            self.list_item_field_exprs.insert(target, field_exprs);
            false
        }
    }

    fn repair_returned_cell_shape(&mut self, result_cell: CellId, span: Span) {
        if self.resolve_cell_field_cells(result_cell).is_none() {
            let _ = self.materialize_list_item_field_cells(result_cell, result_cell, span);
            if self.resolve_cell_field_cells(result_cell).is_none()
                && let Some(source_cell) = self.find_metadata_source_cell(result_cell)
            {
                if let Some(fields) = self.resolve_cell_field_cells(source_cell) {
                    self.cell_field_cells.insert(result_cell, fields);
                } else if let Some(nested_fields) = self.resolve_cell_to_inline_object(source_cell)
                {
                    let inline_map =
                        self.register_inline_object_field_cells(result_cell, &nested_fields, span);
                    if !inline_map.is_empty() {
                        self.cell_field_cells.insert(result_cell, inline_map);
                    }
                } else {
                    let _ = self.materialize_list_item_field_cells(result_cell, source_cell, span);
                }
            }
            if self.resolve_cell_field_cells(result_cell).is_none()
                && let Some(nested_fields) = self.resolve_cell_to_inline_object(result_cell)
            {
                let inline_map =
                    self.register_inline_object_field_cells(result_cell, &nested_fields, span);
                if !inline_map.is_empty() {
                    self.cell_field_cells.insert(result_cell, inline_map);
                }
            }
        }
    }

    /// Inline the list item constructor in the List/map template range.
    /// Creates per-item cells (HOLDs, LINKs, events) so field accesses like
    /// `item.editing` resolve to actual cells instead of FieldAccess IR.
    ///
    /// Returns true if a constructor was inlined.
    fn inline_list_constructor_for_template(
        &mut self,
        source_cell: CellId,
        item_cell: CellId,
        item_name: &str,
        span: Span,
    ) -> bool {
        // Look up constructor, following CellRead/Derived chains through aliases
        // (e.g., PASSED.store.people creates Derived(CellRead(X)) without propagating
        // list_item_constructor). Trace back to find the original constructor.
        let constructor_name = match self.find_list_constructor(source_cell) {
            Some(name) => name,
            None => return false,
        };
        let func_def = match self.func_defs.get(&constructor_name).cloned() {
            Some(def) => def,
            None => return false,
        };

        // Save bindings for constructor params.
        let mut saved_bindings: Vec<(String, Option<CellId>)> = Vec::new();
        for param in &func_def.params {
            saved_bindings.push((param.clone(), self.name_to_cell.get(param).copied()));
        }

        let representative_item_fields = self.representative_list_item_fields(source_cell);

        if self.resolve_cell_field_cells(item_cell).is_none() {
            let _ = self.materialize_list_item_field_cells(item_cell, source_cell, span);
            if self.resolve_cell_field_cells(item_cell).is_none()
                && let Some(nested_fields) = self.resolve_cell_to_inline_object(item_cell)
            {
                let inline_map =
                    self.register_inline_object_field_cells(item_cell, &nested_fields, span);
                if !inline_map.is_empty() {
                    self.cell_field_cells.insert(item_cell, inline_map);
                }
            }
        }

        if let Some(representative_fields) = representative_item_fields.as_ref() {
            let inline_fields = representative_fields.clone();
            for (field_name, _) in &inline_fields {
                self.name_to_cell
                    .remove(&format!("{}.{}", item_name, field_name));
            }
            self.cell_field_cells.remove(&item_cell);
            let inline_map =
                self.register_inline_object_field_cells(item_cell, &inline_fields, span);
            if !inline_map.is_empty() {
                self.cell_field_cells.insert(item_cell, inline_map);
            }
        }

        let initial_item_fields = self.resolve_cell_field_cells(item_cell);
        let mut bound_params = HashSet::new();
        let body_fields = match &func_def.body.node {
            Expression::Object(object) => Some(&object.variables),
            Expression::TaggedObject { object, .. } => Some(&object.variables),
            _ => None,
        };
        let projected_item_fields = body_fields.and_then(|body_fields| {
            let mut projected = Vec::new();
            for field in body_fields {
                let field_name = field.node.name.as_str().to_string();
                let Expression::Alias(Alias::WithoutPassed { parts, .. }) = &field.node.value.node
                else {
                    return None;
                };
                if parts.len() != 1 {
                    return None;
                }
                let param_name = parts[0].as_str();
                if !func_def.params.iter().any(|param| param == param_name) {
                    return None;
                }
                let source_field = initial_item_fields
                    .as_ref()
                    .and_then(|fields| fields.get(&field_name))
                    .copied();
                let preferred_source = source_field
                    .map(|field_cell| self.canonicalize_representative_cell(field_cell))
                    .filter(|resolved| {
                        self.cell_has_concrete_shape(*resolved) || Some(*resolved) != source_field
                    });
                let representative_expr = representative_item_fields.as_ref().and_then(|fields| {
                    fields
                        .iter()
                        .find_map(|(name, expr)| (name == &field_name).then_some(expr.clone()))
                });
                let dynamic_item_field = |field_name: &str| IrExpr::FieldAccess {
                    object: Box::new(IrExpr::CellRead(item_cell)),
                    field: field_name.to_string(),
                };
                if let Some(preferred_source) = preferred_source {
                    if self.cell_has_concrete_shape(preferred_source)
                        || self.find_list_constructor(preferred_source).is_some()
                        || self.find_list_item_field_exprs(preferred_source).is_some()
                    {
                        projected.push((field_name, IrExpr::CellRead(preferred_source)));
                    } else if initial_item_fields
                        .as_ref()
                        .and_then(|fields| fields.get(&field_name))
                        .is_some()
                    {
                        projected.push((field_name.clone(), dynamic_item_field(&field_name)));
                    } else {
                        projected.push((field_name, IrExpr::CellRead(preferred_source)));
                    }
                } else if initial_item_fields
                    .as_ref()
                    .and_then(|fields| fields.get(&field_name))
                    .is_some()
                {
                    projected.push((field_name.clone(), dynamic_item_field(&field_name)));
                } else if let Some(representative_expr) = representative_expr {
                    projected.push((field_name, representative_expr));
                } else if let Some(source_field) = source_field {
                    projected.push((
                        field_name,
                        IrExpr::CellRead(self.canonicalize_representative_cell(source_field)),
                    ));
                } else {
                    return None;
                }
            }
            Some(projected)
        });

        if let Some(projected_fields) = projected_item_fields.as_ref() {
            for (field_name, _) in projected_fields {
                self.name_to_cell
                    .remove(&format!("{}.{}", item_name, field_name));
            }
            self.cell_field_cells.remove(&item_cell);
            let inline_map =
                self.register_inline_object_field_cells(item_cell, projected_fields, span);
            if !inline_map.is_empty() {
                self.cell_field_cells.insert(item_cell, inline_map);
            }
            self.name_to_cell.insert(item_name.to_string(), item_cell);
            for (name, saved) in saved_bindings {
                if let Some(cell) = saved {
                    self.name_to_cell.insert(name, cell);
                } else {
                    self.name_to_cell.remove(&name);
                }
            }
            return true;
        }
        if let Some(body_fields) = body_fields {
            for field in body_fields {
                let field_name = field.node.name.as_str().to_string();
                let Some(param_name) = (match &field.node.value.node {
                    Expression::Alias(Alias::WithoutPassed { parts, .. }) if parts.len() == 1 => {
                        Some(parts[0].as_str().to_string())
                    }
                    _ => None,
                }) else {
                    continue;
                };
                if !func_def.params.iter().any(|param| param == &param_name) {
                    continue;
                }

                let source_field = self
                    .resolve_cell_field_cells(item_cell)
                    .and_then(|item_fields| item_fields.get(&field_name).copied());
                let representative_expr = representative_item_fields.as_ref().and_then(|fields| {
                    fields
                        .iter()
                        .find_map(|(name, expr)| (name == &field_name).then_some(expr.clone()))
                });
                let field_cell = self.alloc_cell(&param_name, span);
                let representative_source =
                    representative_expr.as_ref().and_then(|expr| match expr {
                        IrExpr::CellRead(source_cell) => Some(*source_cell),
                        _ => self.resolve_field_access_expr_to_cell(expr),
                    });
                let preferred_source = source_field
                    .map(|source_field| self.canonicalize_representative_cell(source_field))
                    .filter(|resolved| {
                        self.cell_has_concrete_shape(*resolved) || Some(*resolved) != source_field
                    });
                let bound_source = preferred_source.or(representative_source).or(source_field);
                if let Some(source_field) = bound_source {
                    self.name_to_cell.insert(param_name.clone(), source_field);
                    if let Some(event) = self.cell_events.get(&source_field).copied() {
                        self.cell_events.insert(source_field, event);
                    }
                    if let Some(source_fields) = self.resolve_cell_field_cells(source_field) {
                        for (nested_name, nested_cell) in &source_fields {
                            let dotted = format!("{}.{}", param_name, nested_name);
                            self.name_to_cell.insert(dotted, *nested_cell);
                        }
                    } else if let Some(nested_fields) =
                        self.resolve_cell_to_inline_object(source_field)
                    {
                        let inline_map = self.register_inline_object_field_cells(
                            source_field,
                            &nested_fields,
                            span,
                        );
                        for (nested_name, nested_cell) in &inline_map {
                            let dotted = format!("{}.{}", param_name, nested_name);
                            self.name_to_cell.insert(dotted, *nested_cell);
                        }
                    } else if self.materialize_list_item_field_cells(
                        source_field,
                        source_field,
                        span,
                    ) && let Some(field_map) =
                        self.cell_field_cells.get(&source_field).cloned()
                    {
                        for (nested_name, nested_cell) in &field_map {
                            let dotted = format!("{}.{}", param_name, nested_name);
                            self.name_to_cell.insert(dotted, *nested_cell);
                        }
                    }
                    bound_params.insert(param_name);
                    continue;
                }
                let field_expr = representative_expr
                    .as_ref()
                    .map(|expr| self.reduce_representative_expr(expr))
                    .unwrap_or_else(|| {
                        // Keep per-item values dynamic by reading through the current
                        // template item when no representative field source survives.
                        IrExpr::FieldAccess {
                            object: Box::new(IrExpr::CellRead(item_cell)),
                            field: field_name.clone(),
                        }
                    });
                self.nodes.push(IrNode::Derived {
                    cell: field_cell,
                    expr: field_expr.clone(),
                });
                let namespace_source = match &field_expr {
                    IrExpr::CellRead(source_cell) => Some(
                        preferred_source
                            .or(representative_source)
                            .unwrap_or(*source_cell),
                    ),
                    _ => preferred_source
                        .or_else(|| self.resolve_field_access_expr_to_cell(&field_expr))
                        .or(representative_source),
                };
                if let Some(source_field) = namespace_source {
                    if let Some(event) = self.cell_events.get(&source_field).copied() {
                        self.cell_events.insert(field_cell, event);
                    }
                    let source_field_name = self.cells[source_field.0 as usize].name.clone();
                    if let Some(events) = self.element_events.get(&source_field_name).cloned() {
                        let alias_name = self.cells[field_cell.0 as usize].name.clone();
                        self.element_events.insert(alias_name, events);
                    }
                    if let Some(source_fields) = self.resolve_cell_field_cells(source_field) {
                        let alias_fields = self.build_field_alias_map(field_cell, &source_fields);
                        if !alias_fields.is_empty() {
                            self.cell_field_cells.insert(field_cell, alias_fields);
                        }
                    }
                    if let Some(constructor) = self.find_list_constructor(source_field) {
                        self.list_item_constructor.insert(field_cell, constructor);
                    }
                    if let Some(field_exprs) = self.find_list_item_field_exprs(source_field) {
                        self.list_item_field_exprs.insert(field_cell, field_exprs);
                        let _ =
                            self.materialize_list_item_field_cells(field_cell, source_field, span);
                    }
                }
                self.name_to_cell.insert(param_name.clone(), field_cell);
                bound_params.insert(param_name);
            }
        }

        // Fallback for simple single-parameter constructors: bind the first
        // parameter to the whole current item when no field-based mapping applies.
        if bound_params.is_empty() {
            if let Some(first) = func_def.params.first() {
                self.name_to_cell.insert(first.clone(), item_cell);
                bound_params.insert(first.clone());
            }
        }

        // Bind unhandled params to defaults (False).
        for param in &func_def.params {
            if bound_params.contains(param) {
                continue;
            }
            let default_cell = self.alloc_cell(param, span);
            self.nodes.push(IrNode::Derived {
                cell: default_cell,
                expr: IrExpr::Constant(IrValue::Bool(false)),
            });
            self.name_to_cell.insert(param.clone(), default_cell);
        }

        // Lower constructor body — creates namespace cells in template range.
        // Force objects to use lower_object_store so field cells are always created.
        let cell_start = self.cells.len() as u32;
        let saved_force = self.force_object_store;
        self.force_object_store = true;
        let result_cell = self.lower_expr_to_cell(&func_def.body, item_name);
        self.force_object_store = saved_force;
        let item_root_cell = if self.resolve_cell_field_cells(result_cell).is_some() {
            result_cell
        } else {
            (cell_start..self.cells.len() as u32)
                .rev()
                .map(CellId)
                .find(|cell| self.resolve_cell_field_cells(*cell).is_some())
                .unwrap_or(result_cell)
        };

        if self.resolve_cell_field_cells(item_root_cell).is_none()
            && let Some(nested_fields) = self.resolve_cell_to_inline_object(item_root_cell)
        {
            let inline_map =
                self.register_inline_object_field_cells(item_root_cell, &nested_fields, span);
            if !inline_map.is_empty() {
                self.cell_field_cells.insert(item_root_cell, inline_map);
            }
        }

        if let Some(projected_fields) = projected_item_fields.as_ref() {
            let item_root_name = self.cells[item_root_cell.0 as usize].name.clone();
            for (field_name, _) in projected_fields {
                self.name_to_cell
                    .remove(&format!("{}.{}", item_root_name, field_name));
            }
            self.cell_field_cells.remove(&item_root_cell);
            let inline_map =
                self.register_inline_object_field_cells(item_root_cell, projected_fields, span);
            if !inline_map.is_empty() {
                self.cell_field_cells.insert(item_root_cell, inline_map);
            }
        }

        // Bind item name to the constructor's result cell so template
        // can resolve field accesses (e.g., todo.editing) through it.
        self.name_to_cell
            .insert(item_name.to_string(), item_root_cell);

        if self.resolve_cell_field_cells(item_root_cell).is_none() {
            let _ = self.materialize_list_item_field_cells(item_root_cell, item_root_cell, span);
        }

        // Also propagate concrete field cells to item_cell for bridge use.
        if let Some(fields) = self.resolve_cell_field_cells(item_root_cell) {
            self.cell_field_cells.insert(item_root_cell, fields.clone());
            self.cell_field_cells.insert(item_cell, fields);
        } else if self.materialize_list_item_field_cells(item_cell, item_root_cell, span) {
            if let Some(fields) = self.cell_field_cells.get(&item_cell).cloned() {
                self.cell_field_cells.insert(item_root_cell, fields.clone());
            }
        }

        if let Some(projected_fields) = projected_item_fields.as_ref() {
            for (field_name, _) in projected_fields {
                self.name_to_cell
                    .remove(&format!("{}.{}", item_name, field_name));
            }
            self.cell_field_cells.remove(&item_cell);
            let inline_map =
                self.register_inline_object_field_cells(item_cell, projected_fields, span);
            if !inline_map.is_empty() {
                self.cell_field_cells.insert(item_cell, inline_map);
            }
        }

        if let Some(fields) = self
            .resolve_cell_field_cells(item_root_cell)
            .or_else(|| self.resolve_cell_field_cells(item_cell))
        {
            for (field_name, field_cell) in &fields {
                let dotted = format!("{}.{}", item_name, field_name);
                let alias_cell = self.alloc_cell(&dotted, span);
                self.nodes.push(IrNode::Derived {
                    cell: alias_cell,
                    expr: IrExpr::CellRead(*field_cell),
                });
                self.name_to_cell.insert(dotted, alias_cell);
                self.propagate_list_constructor(*field_cell, alias_cell);
                if let Some(nested_fields) = self.resolve_cell_field_cells(*field_cell) {
                    let alias_fields = self.build_field_alias_map(alias_cell, &nested_fields);
                    if !alias_fields.is_empty() {
                        self.cell_field_cells.insert(alias_cell, alias_fields);
                    }
                }
            }
        } else if let Some(inline_fields) = self
            .resolve_cell_to_inline_object(item_root_cell)
            .or_else(|| self.resolve_cell_to_inline_object(item_cell))
        {
            for (field_name, field_expr) in &inline_fields {
                let dotted = format!("{}.{}", item_name, field_name);
                let alias_cell = if let Some(&existing) = self.name_to_cell.get(&dotted) {
                    existing
                } else {
                    let alias_expr = self.reduce_representative_expr(field_expr);
                    let alias_cell = self.alloc_cell(&dotted, span);
                    self.nodes.push(IrNode::Derived {
                        cell: alias_cell,
                        expr: alias_expr,
                    });
                    alias_cell
                };
                self.name_to_cell.insert(dotted, alias_cell);
                if let Some(source_cell) = self.nodes.iter().rev().find_map(|node| match node {
                    IrNode::Derived {
                        cell,
                        expr: IrExpr::CellRead(source),
                    } if *cell == alias_cell => Some(*source),
                    _ => None,
                }) {
                    self.propagate_list_constructor(source_cell, alias_cell);
                }
                if let Some(nested_fields) = self.resolve_cell_field_cells(alias_cell) {
                    let alias_fields = self.build_field_alias_map(alias_cell, &nested_fields);
                    if !alias_fields.is_empty() {
                        self.cell_field_cells.insert(alias_cell, alias_fields);
                    }
                }
            }
        }

        // Restore constructor param bindings.
        for (name, saved) in saved_bindings {
            if let Some(cell) = saved {
                self.name_to_cell.insert(name, cell);
            } else {
                self.name_to_cell.remove(&name);
            }
        }

        true
    }

    fn finish(self) -> Result<IrProgram, Vec<CompileError>> {
        // Large programs (>50K nodes) use async WASM compilation to bypass
        // Chrome's 8MB synchronous WebAssembly.Module.new() limit.
        if !self.errors.is_empty() {
            return Err(self.errors);
        }
        let mut program = IrProgram {
            cells: self.cells,
            events: self.events,
            nodes: self.nodes,
            document: self.document,
            render_surface: self.render_surface,
            functions: self.functions,
            tag_table: self.tag_table,
            cell_field_cells: self.cell_field_cells,
            list_map_plans: HashMap::new(),
        };
        program.list_map_plans = program.compute_list_map_plans();
        Ok(program)
    }

    fn set_render_root(
        &mut self,
        kind: RenderSurface,
        root: CellId,
        lights: Option<CellId>,
        geometry: Option<CellId>,
    ) {
        self.nodes.push(IrNode::Document {
            kind,
            root,
            lights,
            geometry,
        });
        self.document = Some(root);
        self.render_surface = Some(kind);
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub(super) fn lower(
    ast: &[Spanned<Expression>],
    external_functions: Option<&[ExternalFunction]>,
) -> Result<IrProgram, Vec<CompileError>> {
    let mut ctx = lower_to_ctx(ast, external_functions);
    ctx.lower_function_bodies();
    ctx.finish()
}

fn lower_to_ctx(
    ast: &[Spanned<Expression>],
    external_functions: Option<&[ExternalFunction]>,
) -> Lowerer {
    let mut ctx = Lowerer::new();

    if let Some(ext_fns) = external_functions {
        ctx.register_external_functions(ext_fns);
    }

    let mut top_level_vars: Vec<(&Variable, Span)> = Vec::new();

    for item in ast {
        match &item.node {
            Expression::Variable(var) => {
                let name = var.name.as_str();
                if let Expression::Function {
                    name: fn_name,
                    parameters,
                    body,
                } = &var.value.node
                {
                    let func_id = FuncId(u32::try_from(ctx.name_to_func.len()).unwrap());
                    let fn_name_str = fn_name.as_str().to_string();
                    ctx.name_to_func.insert(fn_name_str.clone(), func_id);
                    ctx.func_defs.insert(
                        fn_name_str,
                        FuncDef {
                            params: parameters
                                .iter()
                                .map(|p| p.node.as_str().to_string())
                                .collect(),
                            body: (**body).clone(),
                        },
                    );
                }

                let cell = ctx.alloc_cell(name, item.span);
                ctx.name_to_cell.insert(name.to_string(), cell);
                top_level_vars.push((var, item.span));
            }
            Expression::Function {
                name: fn_name,
                parameters,
                body,
            } => {
                let func_id = FuncId(u32::try_from(ctx.name_to_func.len()).unwrap());
                let fn_name_str = fn_name.as_str().to_string();
                ctx.name_to_func.insert(fn_name_str.clone(), func_id);
                ctx.func_defs.insert(
                    fn_name_str,
                    FuncDef {
                        params: parameters
                            .iter()
                            .map(|p| p.node.as_str().to_string())
                            .collect(),
                        body: (**body).clone(),
                    },
                );
            }
            _ => {
                ctx.error(
                    item.span,
                    "Top-level expression must be a variable or function definition",
                );
            }
        }
    }

    for (var, span) in &top_level_vars {
        let var_name = var.name.as_str().to_string();
        let cell = ctx.name_to_cell[var.name.as_str()];
        pre_scan_links_in_expr(&var.value.node, &var_name, cell, *span, &mut ctx);
    }

    let lower_trace = env::var("BOON_WASM_LOWER_TRACE").is_ok();
    let mut shape_first_vars = Vec::new();
    let mut remaining_vars = Vec::new();
    for &(var, span) in &top_level_vars {
        if ctx
            .extract_list_constructor_from_value_expr(&var.value.node)
            .is_some()
        {
            shape_first_vars.push((var, span));
        } else {
            remaining_vars.push((var, span));
        }
    }
    shape_first_vars.extend(remaining_vars);

    for (var, span) in &shape_first_vars {
        let name = var.name.as_str();
        let cell = ctx.name_to_cell[name];

        if lower_trace {
            eprintln!("[wasm-lower] var start {name}");
        }

        if let Expression::Function { .. } = &var.value.node {
            ctx.nodes.push(IrNode::Derived {
                cell,
                expr: IrExpr::Constant(IrValue::Void),
            });
            if lower_trace {
                eprintln!("[wasm-lower] var done {name} (fn placeholder)");
            }
            continue;
        }

        ctx.lower_variable(cell, &var.value, *span);
        if lower_trace {
            eprintln!("[wasm-lower] var done {name}");
        }
    }

    ctx
}

// ---------------------------------------------------------------------------
// Expression lowering
// ---------------------------------------------------------------------------

impl Lowerer {
    fn lower_function_bodies(&mut self) {
        let function_count = self.name_to_func.len();
        if function_count == 0 {
            return;
        }

        let lower_trace = env::var("BOON_WASM_LOWER_TRACE").is_ok();
        let runtime_functions = self.find_runtime_lowered_functions();

        let mut lowered: Vec<Option<IrFunction>> = (0..function_count).map(|_| None).collect();
        let func_defs: Vec<(String, FuncDef)> = self
            .func_defs
            .iter()
            .map(|(name, def)| (name.clone(), def.clone()))
            .collect();

        for (fn_name, func_def) in func_defs {
            let Some(&func_id) = self.name_to_func.get(&fn_name) else {
                continue;
            };

            if !runtime_functions.contains(&fn_name) {
                lowered[func_id.0 as usize] = Some(IrFunction {
                    name: fn_name.clone(),
                    params: func_def.params.clone(),
                    param_cells: Vec::new(),
                    body: IrExpr::Constant(IrValue::Void),
                });
                continue;
            }

            if lower_trace {
                eprintln!("[wasm-lower] fn start {fn_name}");
            }

            let saved_names = self.name_to_cell.clone();
            let saved_module = self.current_module.clone();
            let saved_passed = self.current_passed.take();

            let param_cells: Vec<CellId> = func_def
                .params
                .iter()
                .map(|param| {
                    let cell =
                        self.alloc_cell(&format!("__fn.{}.{}", fn_name, param), func_def.body.span);
                    self.name_to_cell.insert(param.clone(), cell);
                    cell
                })
                .collect();

            self.current_module = self.function_modules.get(&fn_name).cloned();
            let body = self.with_preserved_runtime_function_calls(|this| {
                this.lower_expr(&func_def.body.node, func_def.body.span)
            });

            lowered[func_id.0 as usize] = Some(IrFunction {
                name: fn_name.clone(),
                params: func_def.params.clone(),
                param_cells,
                body,
            });

            if lower_trace {
                eprintln!("[wasm-lower] fn done {fn_name}");
            }

            self.name_to_cell = saved_names;
            self.current_module = saved_module;
            self.current_passed = saved_passed;
        }

        self.functions = lowered
            .into_iter()
            .enumerate()
            .map(|(idx, maybe)| {
                maybe.unwrap_or(IrFunction {
                    name: format!("__missing_{idx}"),
                    params: Vec::new(),
                    param_cells: Vec::new(),
                    body: IrExpr::Constant(IrValue::Void),
                })
            })
            .collect();
    }

    fn find_runtime_lowered_functions(&self) -> HashSet<String> {
        let mut call_graph: HashMap<String, HashSet<String>> = HashMap::new();
        for (fn_name, func_def) in &self.func_defs {
            let module = self.function_modules.get(fn_name).map(String::as_str);
            let mut calls = HashSet::new();
            self.collect_called_functions_in_expr(&func_def.body.node, module, &mut calls);
            call_graph.insert(fn_name.clone(), calls);
        }

        let mut runtime_functions = HashSet::new();
        for fn_name in self.func_defs.keys() {
            let mut visiting = HashSet::new();
            if self.function_reaches_itself(fn_name, fn_name, &call_graph, &mut visiting) {
                runtime_functions.insert(fn_name.clone());
            }
        }
        runtime_functions
    }

    fn function_reaches_itself(
        &self,
        start: &str,
        current: &str,
        call_graph: &HashMap<String, HashSet<String>>,
        visiting: &mut HashSet<String>,
    ) -> bool {
        if !visiting.insert(current.to_string()) {
            return false;
        }
        if let Some(callees) = call_graph.get(current) {
            for callee in callees {
                if callee == start
                    || self.function_reaches_itself(start, callee, call_graph, visiting)
                {
                    return true;
                }
            }
        }
        false
    }

    fn cell_has_concrete_shape(&self, cell: CellId) -> bool {
        self.resolve_cell_field_cells(cell).is_some()
            || self.resolve_cell_to_inline_object(cell).is_some()
    }

    fn canonicalize_shape_source_cell(&self, cell: CellId) -> CellId {
        let mut current = cell;
        let mut seen = HashSet::new();
        while seen.insert(current) {
            if self.cell_has_concrete_shape(current) {
                let is_list_shape = self.find_list_constructor(current).is_some()
                    || self.find_list_item_field_exprs(current).is_some();
                if is_list_shape
                    && let Some(next) = self.find_metadata_source_cell(current)
                    && next != current
                {
                    current = next;
                    continue;
                }
                break;
            }
            match self.find_metadata_source_cell(current) {
                Some(next) if next != current => current = next,
                _ => break,
            }
        }
        current
    }

    fn collect_called_functions_in_expr(
        &self,
        expr: &Expression,
        current_module: Option<&str>,
        out: &mut HashSet<String>,
    ) {
        match expr {
            Expression::FunctionCall { path, arguments } => {
                if let Some(fn_name) = self.resolve_called_function_name(path, current_module) {
                    out.insert(fn_name);
                }
                for arg in arguments {
                    if let Some(value) = &arg.node.value {
                        self.collect_called_functions_in_expr(&value.node, current_module, out);
                    }
                }
            }
            Expression::Pipe { from, to } => {
                self.collect_called_functions_in_expr(&from.node, current_module, out);
                self.collect_called_functions_in_expr(&to.node, current_module, out);
            }
            Expression::Latest { inputs } | Expression::List { items: inputs } => {
                for input in inputs {
                    self.collect_called_functions_in_expr(&input.node, current_module, out);
                }
            }
            Expression::Block { variables, output } => {
                for var in variables {
                    self.collect_called_functions_in_expr(
                        &var.node.value.node,
                        current_module,
                        out,
                    );
                }
                self.collect_called_functions_in_expr(&output.node, current_module, out);
            }
            Expression::Object(obj) => {
                for var in &obj.variables {
                    self.collect_called_functions_in_expr(
                        &var.node.value.node,
                        current_module,
                        out,
                    );
                }
            }
            Expression::TaggedObject { object, .. } => {
                for var in &object.variables {
                    self.collect_called_functions_in_expr(
                        &var.node.value.node,
                        current_module,
                        out,
                    );
                }
            }
            Expression::Then { body } | Expression::Flush { value: body } => {
                self.collect_called_functions_in_expr(&body.node, current_module, out);
            }
            Expression::When { arms } => {
                for arm in arms {
                    self.collect_called_functions_in_expr(&arm.body.node, current_module, out);
                }
            }
            Expression::While { arms } => {
                for arm in arms {
                    self.collect_called_functions_in_expr(&arm.body.node, current_module, out);
                }
            }
            Expression::PostfixFieldAccess { expr, .. } | Expression::Spread { value: expr } => {
                self.collect_called_functions_in_expr(&expr.node, current_module, out);
            }
            Expression::Function { body, .. } => {
                self.collect_called_functions_in_expr(&body.node, current_module, out);
            }
            Expression::Variable(var) => {
                self.collect_called_functions_in_expr(&var.value.node, current_module, out);
            }
            Expression::Comparator(comparator) => match comparator {
                crate::parser::static_expression::Comparator::Equal {
                    operand_a,
                    operand_b,
                }
                | crate::parser::static_expression::Comparator::NotEqual {
                    operand_a,
                    operand_b,
                }
                | crate::parser::static_expression::Comparator::Greater {
                    operand_a,
                    operand_b,
                }
                | crate::parser::static_expression::Comparator::GreaterOrEqual {
                    operand_a,
                    operand_b,
                }
                | crate::parser::static_expression::Comparator::Less {
                    operand_a,
                    operand_b,
                }
                | crate::parser::static_expression::Comparator::LessOrEqual {
                    operand_a,
                    operand_b,
                } => {
                    self.collect_called_functions_in_expr(&operand_a.node, current_module, out);
                    self.collect_called_functions_in_expr(&operand_b.node, current_module, out);
                }
            },
            Expression::ArithmeticOperator(operator) => match operator {
                crate::parser::static_expression::ArithmeticOperator::Negate { operand } => {
                    self.collect_called_functions_in_expr(&operand.node, current_module, out);
                }
                crate::parser::static_expression::ArithmeticOperator::Add {
                    operand_a,
                    operand_b,
                }
                | crate::parser::static_expression::ArithmeticOperator::Subtract {
                    operand_a,
                    operand_b,
                }
                | crate::parser::static_expression::ArithmeticOperator::Multiply {
                    operand_a,
                    operand_b,
                }
                | crate::parser::static_expression::ArithmeticOperator::Divide {
                    operand_a,
                    operand_b,
                } => {
                    self.collect_called_functions_in_expr(&operand_a.node, current_module, out);
                    self.collect_called_functions_in_expr(&operand_b.node, current_module, out);
                }
            },
            Expression::TextLiteral { parts, .. } => {
                for part in parts {
                    if let crate::parser::static_expression::TextPart::Interpolation { .. } = part {
                        // Interpolations reference variables, not nested expressions.
                    }
                }
            }
            Expression::Alias(_)
            | Expression::Literal(_)
            | Expression::Map { .. }
            | Expression::Link
            | Expression::LinkSetter { .. }
            | Expression::FieldAccess { .. }
            | Expression::Skip => {}
            Expression::Hold { body, .. } => {
                self.collect_called_functions_in_expr(&body.node, current_module, out);
            }
            Expression::Bits { size } => {
                self.collect_called_functions_in_expr(&size.node, current_module, out);
            }
            Expression::Memory { address } => {
                self.collect_called_functions_in_expr(&address.node, current_module, out);
            }
            Expression::Bytes { data } => {
                for item in data {
                    self.collect_called_functions_in_expr(&item.node, current_module, out);
                }
            }
        }
    }

    fn resolve_called_function_name(
        &self,
        path: &[crate::parser::StrSlice],
        current_module: Option<&str>,
    ) -> Option<String> {
        let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
        let effective =
            if path_strs.len() == 3 && path_strs[0] == "Scene" && path_strs[1] == "Element" {
                vec!["Element", path_strs[2]]
            } else {
                path_strs
            };

        let resolved = match effective.as_slice() {
            [name] => {
                if self.func_defs.contains_key(*name) {
                    Some((*name).to_string())
                } else if let Some(module) = current_module {
                    let qualified = format!("{module}/{name}");
                    self.func_defs.contains_key(&qualified).then_some(qualified)
                } else {
                    None
                }
            }
            [module, name] => {
                let qualified = format!("{module}/{name}");
                self.func_defs.contains_key(&qualified).then_some(qualified)
            }
            _ => None,
        };

        resolved
    }

    fn with_preserved_runtime_function_calls<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        let saved = self.preserve_runtime_function_calls;
        self.preserve_runtime_function_calls = true;
        let result = f(self);
        self.preserve_runtime_function_calls = saved;
        result
    }

    /// Lower a top-level variable's value expression and emit the appropriate IrNode.
    fn lower_variable(&mut self, cell: CellId, value: &Spanned<Expression>, var_span: Span) {
        // Get variable name for LINK resolution.
        let var_name = self.cells[cell.0 as usize].name.clone();

        // Set current_var_name so elements inside function calls inherit LINK events.
        let saved_var_name = self.current_var_name.take();
        self.current_var_name = Some(var_name.clone());

        match &value.node {
            // --- Pipe chains ---
            Expression::Pipe { from, to } => {
                self.lower_pipe(cell, from, to, var_span);
            }

            // --- LATEST ---
            Expression::Latest { inputs } => {
                self.lower_latest(cell, inputs, var_span);
            }

            // --- Element function calls: create Element node directly on this cell ---
            Expression::FunctionCall { path, arguments } => {
                let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                // Scene/Element/* paths are treated like Element/* paths
                let is_element = path_strs.first() == Some(&"Element")
                    || (path_strs.len() == 3
                        && path_strs[0] == "Scene"
                        && path_strs[1] == "Element");
                if is_element {
                    // Remap Scene/Element/* → Element/*
                    let remapped: Vec<&str>;
                    let effective = if path_strs.len() == 3 && path_strs[0] == "Scene" {
                        remapped = vec!["Element", path_strs[2]];
                        &remapped
                    } else {
                        &path_strs
                    };
                    self.lower_element_call(cell, &var_name, effective, arguments, value.span);
                } else {
                    let mut expr = self.lower_expr(&value.node, value.span);
                    if let IrExpr::CellRead(source_cell) = expr {
                        let canonical = if var_name.contains('.') {
                            self.canonicalize_representative_cell(source_cell)
                        } else {
                            self.canonicalize_shape_source_cell(source_cell)
                        };
                        if canonical != source_cell {
                            expr = IrExpr::CellRead(canonical);
                        } else {
                            expr = IrExpr::CellRead(source_cell);
                        }
                    }
                    if matches!(&expr, IrExpr::ListConstruct(_)) {
                        if let Some(constructor_name) = self.pending_list_constructor.take() {
                            self.list_item_constructor.insert(cell, constructor_name);
                        }
                    }
                    if let Some(fields) = self.extract_list_item_field_exprs_from_expr(&expr) {
                        self.list_item_field_exprs.insert(cell, fields);
                        let _ = self.materialize_list_item_field_cells(cell, cell, value.span);
                    }
                    if let Some(constructor_name) =
                        self.extract_list_constructor_from_value_expr(&value.node)
                    {
                        self.list_item_constructor.insert(cell, constructor_name);
                    }
                    self.propagate_expr_field_cells(cell, &expr);
                    let metadata_source = match &expr {
                        IrExpr::CellRead(src) => Some(*src),
                        _ => self.resolve_field_access_expr_to_cell(&expr),
                    };
                    if let Some(src) = metadata_source {
                        self.propagate_list_constructor(src, cell);
                    }
                    self.nodes.push(IrNode::Derived { cell, expr });
                }
            }

            // --- Object-as-store pattern ---
            Expression::Object(obj) => {
                self.lower_object_store(cell, obj, var_span);
            }

            // --- Simple expression → Derived ---
            _ => {
                let mut expr = self.lower_expr(&value.node, value.span);
                if let IrExpr::CellRead(source_cell) = expr {
                    let canonical = if var_name.contains('.') {
                        self.canonicalize_representative_cell(source_cell)
                    } else {
                        self.canonicalize_shape_source_cell(source_cell)
                    };
                    if canonical != source_cell {
                        expr = IrExpr::CellRead(canonical);
                    } else {
                        expr = IrExpr::CellRead(source_cell);
                    }
                }
                // Consume pending_list_constructor for top-level LIST variables.
                if matches!(&expr, IrExpr::ListConstruct(_)) {
                    if let Some(constructor_name) = self.pending_list_constructor.take() {
                        self.list_item_constructor.insert(cell, constructor_name);
                    }
                }
                if let Some(fields) = self.extract_list_item_field_exprs_from_expr(&expr) {
                    self.list_item_field_exprs.insert(cell, fields);
                    let _ = self.materialize_list_item_field_cells(cell, cell, value.span);
                }
                self.propagate_expr_field_cells(cell, &expr);
                let metadata_source = match &expr {
                    IrExpr::CellRead(src) => Some(*src),
                    _ => self.resolve_field_access_expr_to_cell(&expr),
                };
                if let Some(src) = metadata_source {
                    self.propagate_list_constructor(src, cell);
                }
                self.nodes.push(IrNode::Derived { cell, expr });
            }
        }

        // Restore previous current_var_name.
        self.current_var_name = saved_var_name;
    }

    /// Lower an Element/* function call, creating the Element node directly on the given cell.
    fn lower_element_call(
        &mut self,
        cell: CellId,
        var_name: &str,
        path_strs: &[&str],
        arguments: &[Spanned<Argument>],
        span: Span,
    ) {
        let links = self.extract_links_for_element(var_name);
        let hovered_cell = self.name_to_cell.get("element.hovered").copied();

        match path_strs {
            ["Element", "button"] => {
                let label = self.find_arg_expr(arguments, "label", span);
                let style = self.find_arg_expr_or_default(arguments, "style");
                self.nodes.push(IrNode::Element {
                    cell,
                    kind: ElementKind::Button { label, style },
                    links,
                    hovered_cell,
                });
            }
            ["Element", "text_input"] => {
                let placeholder = arguments
                    .iter()
                    .find(|a| a.node.name.as_str() == "placeholder")
                    .and_then(|a| a.node.value.as_ref())
                    .map(|v| self.lower_expr(&v.node, v.span));
                let style = self.find_arg_expr_or_default(arguments, "style");
                let focus = self.has_element_bool_field(arguments, "focus")
                    || self.has_top_level_bool_arg(arguments, "focus");
                // Lower the `text` argument to get the cell providing reactive text.
                let text_cell = arguments
                    .iter()
                    .find(|a| a.node.name.as_str() == "text")
                    .and_then(|a| a.node.value.as_ref())
                    .map(|v| self.lower_expr_to_cell(v, "text_input_text"));
                self.nodes.push(IrNode::Element {
                    cell,
                    kind: ElementKind::TextInput {
                        placeholder,
                        style,
                        focus,
                        text_cell,
                    },
                    links,
                    hovered_cell,
                });
            }
            ["Element", "checkbox"] => {
                let style = self.find_arg_expr_or_default(arguments, "style");
                // Lower the `checked` argument to a CellId if present.
                let checked = arguments
                    .iter()
                    .find(|a| a.node.name.as_str() == "checked")
                    .and_then(|a| a.node.value.as_ref())
                    .map(|v| {
                        let expr = self.lower_expr(&v.node, v.span);
                        match expr {
                            IrExpr::CellRead(c) => c,
                            other => {
                                let c = self.alloc_cell("checkbox_checked", span);
                                self.nodes.push(IrNode::Derived {
                                    cell: c,
                                    expr: other,
                                });
                                c
                            }
                        }
                    });
                // Lower the `icon` argument to a CellId if present.
                let icon = arguments
                    .iter()
                    .find(|a| a.node.name.as_str() == "icon")
                    .and_then(|a| a.node.value.as_ref())
                    .map(|v| {
                        let expr = self.lower_expr(&v.node, v.span);
                        match expr {
                            IrExpr::CellRead(c) => c,
                            other => {
                                let c = self.alloc_cell("checkbox_icon", span);
                                self.nodes.push(IrNode::Derived {
                                    cell: c,
                                    expr: other,
                                });
                                c
                            }
                        }
                    });
                self.nodes.push(IrNode::Element {
                    cell,
                    kind: ElementKind::Checkbox {
                        checked,
                        style,
                        icon,
                    },
                    links,
                    hovered_cell,
                });
            }
            ["Element", "stripe"] => {
                let mut direction = self.find_arg_expr_or_default(arguments, "direction");
                let mut gap =
                    self.find_arg_expr_or(arguments, "gap", IrExpr::Constant(IrValue::Number(0.0)));
                // Fall back to extracting from nested style object
                if matches!(direction, IrExpr::Constant(IrValue::Void)) {
                    if let Some(d) = self.find_field_in_arg_object(arguments, "style", "direction")
                    {
                        direction = d;
                    }
                }
                if matches!(gap, IrExpr::Constant(IrValue::Number(n)) if n == 0.0) {
                    if let Some(g) = self.find_field_in_arg_object(arguments, "style", "gap") {
                        gap = g;
                    }
                }
                let style = self.find_arg_expr_or_default(arguments, "style");
                let element_arg = self.find_arg_expr_or_default(arguments, "element");

                let items_expr = self.find_arg_expr_or_default(arguments, "items");
                let items_cell = self.alloc_cell("stripe_items", span);
                self.nodes.push(IrNode::Derived {
                    cell: items_cell,
                    expr: items_expr,
                });

                self.nodes.push(IrNode::Element {
                    cell,
                    kind: ElementKind::Stripe {
                        direction,
                        items: items_cell,
                        gap,
                        style,
                        element_settings: element_arg,
                    },
                    links,
                    hovered_cell,
                });
            }
            ["Element", "container"] => {
                let child_expr = self.find_arg_expr_or_default(arguments, "child");
                let child_cell = self.alloc_cell("container_child", span);
                self.nodes.push(IrNode::Derived {
                    cell: child_cell,
                    expr: child_expr,
                });
                let style = self.find_arg_expr_or_default(arguments, "style");
                self.nodes.push(IrNode::Element {
                    cell,
                    kind: ElementKind::Container {
                        child: child_cell,
                        style,
                    },
                    links,
                    hovered_cell,
                });
            }
            ["Element", "label"] => {
                let label = self.find_arg_expr(arguments, "label", span);
                let style = self.find_arg_expr_or_default(arguments, "style");
                self.nodes.push(IrNode::Element {
                    cell,
                    kind: ElementKind::Label { label, style },
                    links,
                    hovered_cell,
                });
            }
            ["Element", "stack"] => {
                let style = self.find_arg_expr_or_default(arguments, "style");
                let layers_expr = self.find_arg_expr_or_default(arguments, "layers");
                let layers_cell = self.alloc_cell("stack_layers", span);
                self.nodes.push(IrNode::Derived {
                    cell: layers_cell,
                    expr: layers_expr,
                });
                self.nodes.push(IrNode::Element {
                    cell,
                    kind: ElementKind::Stack {
                        layers: layers_cell,
                        style,
                    },
                    links,
                    hovered_cell,
                });
            }
            ["Element", "link"] => {
                let url = self.find_arg_expr_or_default(arguments, "url");
                let label = self.find_arg_expr(arguments, "label", span);
                let style = self.find_arg_expr_or_default(arguments, "style");
                self.nodes.push(IrNode::Element {
                    cell,
                    kind: ElementKind::Link { url, label, style },
                    links,
                    hovered_cell,
                });
            }
            ["Element", "paragraph"] => {
                // Accept both "content" (singular) and "contents" (plural).
                let content = self.with_preserved_runtime_function_calls(|this| {
                    arguments
                        .iter()
                        .find(|a| {
                            let n = a.node.name.as_str();
                            n == "content" || n == "contents"
                        })
                        .and_then(|a| a.node.value.as_ref())
                        .map(|v| this.lower_expr(&v.node, v.span))
                        .unwrap_or_else(|| {
                            this.error(
                                span,
                                "Element/paragraph requires a 'content' or 'contents' argument",
                            );
                            IrExpr::Constant(IrValue::Void)
                        })
                });
                let style = self.find_arg_expr_or_default(arguments, "style");
                self.nodes.push(IrNode::Element {
                    cell,
                    kind: ElementKind::Paragraph { content, style },
                    links,
                    hovered_cell,
                });
            }
            ["Element", "block"] => {
                let child_expr = self.find_arg_expr_or_default(arguments, "child");
                let child_cell = self.alloc_cell("block_child", span);
                self.nodes.push(IrNode::Derived {
                    cell: child_cell,
                    expr: child_expr,
                });
                let style = self.find_arg_expr_or_default(arguments, "style");
                self.nodes.push(IrNode::Element {
                    cell,
                    kind: ElementKind::Block {
                        child: child_cell,
                        style,
                    },
                    links,
                    hovered_cell,
                });
            }
            ["Element", "text"] => {
                let label = self.with_preserved_runtime_function_calls(|this| {
                    this.find_arg_expr(arguments, "text", span)
                });
                let style = self.find_arg_expr_or_default(arguments, "style");
                self.nodes.push(IrNode::Element {
                    cell,
                    kind: ElementKind::Text { label, style },
                    links,
                    hovered_cell,
                });
            }
            ["Element", "slider"] => {
                let style = self.find_arg_expr_or_default(arguments, "style");
                let value_cell = arguments
                    .iter()
                    .find(|a| a.node.name.as_str() == "value")
                    .and_then(|a| a.node.value.as_ref())
                    .and_then(|v| {
                        let expr = self.lower_expr(&v.node, v.span);
                        match expr {
                            IrExpr::CellRead(c) => Some(c),
                            _ => None,
                        }
                    });
                let min = self.find_arg_number(arguments, "min").unwrap_or(0.0);
                let max = self.find_arg_number(arguments, "max").unwrap_or(100.0);
                let step = self.find_arg_number(arguments, "step").unwrap_or(1.0);
                self.nodes.push(IrNode::Element {
                    cell,
                    kind: ElementKind::Slider {
                        style,
                        value_cell,
                        min,
                        max,
                        step,
                    },
                    links,
                    hovered_cell,
                });
            }
            ["Element", "select"] => {
                let style = self.find_arg_expr_or_default(arguments, "style");
                let options = self.extract_select_options(arguments);
                let selected = arguments
                    .iter()
                    .find(|a| a.node.name.as_str() == "selected")
                    .and_then(|a| a.node.value.as_ref())
                    .map(|v| self.lower_expr(&v.node, v.span));
                self.nodes.push(IrNode::Element {
                    cell,
                    kind: ElementKind::Select {
                        style,
                        options,
                        selected,
                    },
                    links,
                    hovered_cell,
                });
            }
            ["Element", "svg"] => {
                let style = self.find_arg_expr_or_default(arguments, "style");
                let children_expr = self.find_arg_expr_or_default(arguments, "children");
                let children_cell = self.alloc_cell("svg_children", span);
                self.nodes.push(IrNode::Derived {
                    cell: children_cell,
                    expr: children_expr,
                });
                self.nodes.push(IrNode::Element {
                    cell,
                    kind: ElementKind::Svg {
                        style,
                        children: children_cell,
                    },
                    links,
                    hovered_cell,
                });
            }
            ["Element", "svg_circle"] => {
                let cx =
                    self.find_arg_expr_or(arguments, "cx", IrExpr::Constant(IrValue::Number(0.0)));
                let cy =
                    self.find_arg_expr_or(arguments, "cy", IrExpr::Constant(IrValue::Number(0.0)));
                let r =
                    self.find_arg_expr_or(arguments, "r", IrExpr::Constant(IrValue::Number(20.0)));
                let style = self.find_arg_expr_or_default(arguments, "style");
                self.nodes.push(IrNode::Element {
                    cell,
                    kind: ElementKind::SvgCircle { cx, cy, r, style },
                    links,
                    hovered_cell,
                });
            }
            _ => {
                // Unknown element type — treat as a generic custom call.
                let expr = self.lower_generic_function_call(arguments, span);
                self.nodes.push(IrNode::Derived { cell, expr });
            }
        }
    }

    /// Lower a generic function call (for unknown function calls in element context).
    fn lower_generic_function_call(
        &mut self,
        arguments: &[Spanned<Argument>],
        span: Span,
    ) -> IrExpr {
        IrExpr::Constant(IrValue::Void)
    }

    /// Flatten an object literal into individual cells for each field.
    ///
    /// The "object-as-store" pattern: `store: [field_a: ..., field_b: ...]`
    /// Each field becomes a separate cell with a dotted name (e.g., `store.field_a`).
    /// Field names are registered in `name_to_cell` for intra-object references.
    /// LINK placeholder fields get pre-allocated events.
    fn lower_object_store(
        &mut self,
        parent_cell: CellId,
        obj: &crate::parser::static_expression::Object,
        span: Span,
    ) {
        let parent_name = self.cells[parent_cell.0 as usize].name.clone();

        // Collect explicit field names so we know which spread fields to skip.
        let explicit_names: std::collections::HashSet<String> = obj
            .variables
            .iter()
            .filter(|v| !v.node.name.is_empty())
            .map(|v| v.node.name.as_str().to_string())
            .collect();

        // Resolve spread sources: lower spread value expressions, look up their
        // cell_field_cells, and collect fields not overridden by explicit fields.
        // Spreads are processed first so explicit fields can override them.
        let mut spread_field_cells: Vec<(String, CellId)> = Vec::new();
        for v in &obj.variables {
            if !v.node.name.is_empty() {
                continue; // Not a spread entry
            }
            // Lower the spread value (e.g., `surface_variant_base`).
            let spread_expr = self.lower_expr(&v.node.value.node, v.node.value.span);
            if let IrExpr::CellRead(spread_cell) = spread_expr {
                if let Some(field_map) = self.resolve_cell_field_cells(spread_cell) {
                    for (name, field_cell) in &field_map {
                        if !explicit_names.contains(name) {
                            // Create an alias cell that reads the spread source's field.
                            let dotted = format!("{}.{}", parent_name, name);
                            let alias_cell = self.alloc_cell(&dotted, v.span);
                            self.nodes.push(IrNode::Derived {
                                cell: alias_cell,
                                expr: IrExpr::CellRead(*field_cell),
                            });
                            // Propagate cell_field_cells for nested namespaces (e.g., color).
                            if let Some(sub_fields) = self.cell_field_cells.get(field_cell).cloned()
                            {
                                self.cell_field_cells.insert(alias_cell, sub_fields);
                            }
                            if let Some(constructor) = self.find_list_constructor(*field_cell) {
                                self.list_item_constructor.insert(alias_cell, constructor);
                            }
                            self.name_to_cell.insert(dotted, alias_cell);
                            self.name_to_cell.insert(name.clone(), alias_cell);
                            spread_field_cells.push((name.clone(), alias_cell));
                        }
                    }
                }
            }
        }

        // First pass: allocate cells for all explicit fields and register dotted names.
        // IMPORTANT: We save and defer short field name registration to avoid
        // shadowing outer scope bindings. E.g., `title: title |> HOLD { ... }`
        // where "title" is both a field name AND a parameter from the enclosing
        // function. If we register the short name now, the Pipe's `from` expression
        // resolves to the field cell (self-reference) instead of the parameter.
        let mut field_cells = Vec::new();
        let mut saved_short_bindings: Vec<(String, Option<CellId>)> = Vec::new();
        for v in &obj.variables {
            if v.node.name.is_empty() {
                continue; // Skip spread entries — handled above
            }
            let field_name = v.node.name.as_str().to_string();
            let dotted_name = format!("{}.{}", parent_name, field_name);
            let field_cell = self.alloc_cell(&dotted_name, v.span);

            // Save existing short name binding (may be a parameter from outer scope).
            let saved = self.name_to_cell.get(&field_name).copied();
            saved_short_bindings.push((field_name.clone(), saved));

            // Only register the dotted name in the first pass.
            // Short names without an outer binding are pre-registered so
            // sibling fields can forward-reference them. The current field's
            // own short name is temporarily removed during its lowering below
            // to avoid accidental self-reference.
            self.name_to_cell.insert(dotted_name, field_cell);
            if saved.is_none() {
                self.name_to_cell.insert(field_name.clone(), field_cell);
            }
            if let Some(constructor_name) =
                self.extract_list_constructor_from_value_expr(&v.node.value.node)
            {
                self.list_item_constructor
                    .insert(field_cell, constructor_name);
            }

            field_cells.push((field_name, field_cell, saved.is_none()));
        }

        // Second pass: lower each field's value, then register short name.
        // At this point, short names still point to outer scope (e.g., function params).
        // Dotted names (e.g., "object.title") are available for cross-field references.
        // IMPORTANT: Register the short name AFTER lowering the field's value to avoid
        // self-reference when a field name shadows a parameter (e.g., `title: title |> HOLD`).
        let mut field_idx = 0;
        for v in obj.variables.iter() {
            if v.node.name.is_empty() {
                continue; // Skip spread entries
            }
            let (field_name, field_cell, pre_registered_short) = &field_cells[field_idx];
            field_idx += 1;

            if *pre_registered_short {
                self.name_to_cell.remove(field_name);
            }

            if matches!(v.node.value.node, Expression::Link) {
                // LINK placeholder — pre-allocate events and data cells.
                self.pre_allocate_link_events(*field_cell, v.span);
                self.nodes.push(IrNode::Derived {
                    cell: *field_cell,
                    expr: IrExpr::Constant(IrValue::Void),
                });
            } else {
                self.lower_variable(*field_cell, &v.node.value, v.span);
                if Self::expr_may_carry_namespace_shape(&v.node.value.node) {
                    if let Some(source_cell) = self.nodes.iter().rev().find_map(|node| match node {
                        IrNode::Derived {
                            cell,
                            expr: IrExpr::CellRead(source),
                        } if *cell == *field_cell => Some(*source),
                        IrNode::PipeThrough { cell, source } if *cell == *field_cell => {
                            Some(*source)
                        }
                        _ => None,
                    }) {
                        let canonical_source = self.canonicalize_shape_source_cell(
                            self.canonicalize_representative_cell(source_cell),
                        );
                        if canonical_source != source_cell {
                            self.nodes.push(IrNode::Derived {
                                cell: *field_cell,
                                expr: IrExpr::CellRead(canonical_source),
                            });
                        }
                        self.propagate_list_constructor(canonical_source, *field_cell);
                        if let Some(event) = self.cell_events.get(&canonical_source).copied() {
                            self.cell_events.insert(*field_cell, event);
                        }
                        if let Some(source_fields) = self.resolve_cell_field_cells(canonical_source)
                        {
                            let alias_fields =
                                self.build_field_alias_map(*field_cell, &source_fields);
                            if !alias_fields.is_empty() {
                                self.cell_field_cells.insert(*field_cell, alias_fields);
                            }
                        } else if let Some(nested_fields) =
                            self.resolve_cell_to_inline_object(canonical_source)
                        {
                            let inline_map = self.register_inline_object_field_cells(
                                *field_cell,
                                &nested_fields,
                                v.span,
                            );
                            if !inline_map.is_empty() {
                                self.cell_field_cells.insert(*field_cell, inline_map);
                            }
                        } else if let Some(field_exprs) =
                            self.find_list_item_field_exprs(canonical_source)
                        {
                            self.list_item_field_exprs.insert(*field_cell, field_exprs);
                            let _ = self.materialize_list_item_field_cells(
                                *field_cell,
                                canonical_source,
                                v.span,
                            );
                        }
                    }
                }
            }
            // Register the short field name AFTER lowering so this field's own
            // value doesn't self-reference, but subsequent fields CAN reference it.
            self.name_to_cell.insert(field_name.clone(), *field_cell);
        }

        // Register field cells: merge spread fields (first) and explicit fields (override).
        let mut field_map: HashMap<String, CellId> = spread_field_cells.iter().cloned().collect();
        for (name, cell, _) in &field_cells {
            field_map.insert(name.clone(), *cell);
        }
        self.cell_field_cells.insert(parent_cell, field_map);

        // Parent cell is void (it's just a namespace).
        self.nodes.push(IrNode::Derived {
            cell: parent_cell,
            expr: IrExpr::Constant(IrValue::Void),
        });
    }

    /// Pre-allocate common events and data cells for a LINK placeholder.
    ///
    /// Creates events for press, key_down, change and associated data cells
    /// for event payloads (key_down.key, change.text) and element properties (.text).
    fn pre_allocate_link_events(&mut self, cell: CellId, span: Span) {
        let cell_name = self.cells[cell.0 as usize].name.clone();
        let mut field_map = self
            .cell_field_cells
            .get(&cell)
            .cloned()
            .unwrap_or_default();

        let alloc_void_cell = |this: &mut Self, name: String| -> CellId {
            if let Some(&existing) = this.name_to_cell.get(&name) {
                existing
            } else {
                let cell = this.alloc_cell(&name, span);
                this.name_to_cell.insert(name, cell);
                this.nodes.push(IrNode::Derived {
                    cell,
                    expr: IrExpr::Constant(IrValue::Void),
                });
                cell
            }
        };

        // Pre-allocate common event types.
        let common_events = [
            "press",
            "click",
            "key_down",
            "change",
            "blur",
            "focus",
            "double_click",
        ];
        let events = if let Some(existing) = self.element_events.get(&cell_name).cloned() {
            existing
        } else {
            let mut events = HashMap::new();
            for event_name in &common_events {
                let event_id = self.alloc_event(
                    &format!("{}.{}", cell_name, event_name),
                    EventSource::Link {
                        element: cell,
                        event_name: event_name.to_string(),
                    },
                    span,
                );
                events.insert(event_name.to_string(), event_id);
            }
            self.element_events
                .insert(cell_name.clone(), events.clone());
            events
        };

        let event_ns_name = format!("{}.event", cell_name);
        let event_ns_cell = alloc_void_cell(self, event_ns_name);
        let key_down_ns_name = format!("{}.event.key_down", cell_name);
        let key_down_ns_cell = alloc_void_cell(self, key_down_ns_name);
        let change_ns_name = format!("{}.event.change", cell_name);
        let change_ns_cell = alloc_void_cell(self, change_ns_name);

        // Pre-allocate data cells for event payloads.
        // key_down.key → CellId (stores the key tag, e.g., Enter, Escape)
        let key_cell_name = format!("{}.event.key_down.key", cell_name);
        let key_cell = alloc_void_cell(self, key_cell_name);
        // Register event for the key cell so WHEN can trigger on key changes.
        if let Some(&event_id) = self
            .element_events
            .get(&cell_name)
            .and_then(|evts| evts.get("key_down"))
        {
            self.cell_events.insert(key_cell, event_id);
            // Add key_cell as payload for the key_down event.
            self.events[event_id.0 as usize]
                .payload_cells
                .push(key_cell);
        }

        // change.text / change.value → CellIds (stores the changed payload)
        let change_text_name = format!("{}.event.change.text", cell_name);
        let change_text_cell = alloc_void_cell(self, change_text_name);
        let change_value_name = format!("{}.event.change.value", cell_name);
        let change_value_cell = alloc_void_cell(self, change_value_name);
        // Add both payload cells for the change event so text inputs and sliders
        // can read the same event through different field names.
        if let Some(&event_id) = self
            .element_events
            .get(&cell_name)
            .and_then(|evts| evts.get("change"))
        {
            self.cell_events.insert(change_text_cell, event_id);
            self.cell_events.insert(change_value_cell, event_id);
            self.events[event_id.0 as usize]
                .payload_cells
                .push(change_text_cell);
            self.events[event_id.0 as usize]
                .payload_cells
                .push(change_value_cell);
        }

        // .text → CellId (current text value of the input)
        let text_name = format!("{}.text", cell_name);
        let text_cell = alloc_void_cell(self, text_name);

        let mut event_fields = HashMap::new();
        event_fields.insert("key_down".to_string(), key_down_ns_cell);
        event_fields.insert("change".to_string(), change_ns_cell);
        for event_name in ["press", "click", "blur", "focus", "double_click"] {
            let event_cell_name = format!("{}.event.{}", cell_name, event_name);
            let event_cell = alloc_void_cell(self, event_cell_name);
            if let Some(&event_id) = self
                .element_events
                .get(&cell_name)
                .and_then(|evts| evts.get(event_name))
            {
                self.cell_events.insert(event_cell, event_id);
            }
            event_fields.insert(event_name.to_string(), event_cell);
        }
        self.cell_field_cells.insert(event_ns_cell, event_fields);

        let mut key_down_fields = HashMap::new();
        key_down_fields.insert("key".to_string(), key_cell);
        self.cell_field_cells
            .insert(key_down_ns_cell, key_down_fields);

        let mut change_fields = HashMap::new();
        change_fields.insert("text".to_string(), change_text_cell);
        change_fields.insert("value".to_string(), change_value_cell);
        self.cell_field_cells.insert(change_ns_cell, change_fields);

        field_map.insert("event".to_string(), event_ns_cell);
        field_map.insert("text".to_string(), text_cell);
        self.cell_field_cells.insert(cell, field_map);
    }

    /// Process the `element` argument of an Element/* call to enable self-references.
    ///
    /// When an Element call has `element: [event: [press: LINK], hovered: LINK]`,
    /// references like `element.hovered` or `element.event.change.text` need to resolve.
    /// This method:
    /// 1. Registers "element" as a cell name in `name_to_cell`
    /// 2. Pre-allocates events from nested LINK fields
    /// 3. Creates data cells for event payloads and properties
    ///
    /// Returns the saved binding for "element" so the caller can restore it after.
    fn process_element_self_ref(
        &mut self,
        arguments: &[Spanned<Argument>],
        span: Span,
    ) -> SavedElementBindings {
        // Always save ALL current element.* bindings first, so that
        // restore_element_self_ref can properly restore them even if
        // this Element call has no `element:` argument.
        let mut saved_bindings = Vec::new();
        let keys: Vec<String> = self
            .name_to_cell
            .keys()
            .filter(|k| *k == "element" || k.starts_with("element."))
            .cloned()
            .collect();
        for k in &keys {
            saved_bindings.push((k.clone(), self.name_to_cell.get(k).copied()));
        }
        let saved_events = self.element_events.get("element").cloned();

        // Check for `element:` argument BEFORE clearing parent bindings.
        // If no `element:` arg, keep parent bindings visible so this element's
        // arguments can reference them (e.g., a nested Scene/Element/text can
        // reference the parent button's `element.hovered` in its style argument).
        let elem_arg = arguments.iter().find(|a| a.node.name.as_str() == "element");
        let elem_obj = match elem_arg {
            Some(arg) => match arg.node.value.as_ref() {
                Some(val) => match &val.node {
                    Expression::Object(obj) => Some(obj),
                    _ => None,
                },
                None => None,
            },
            None => {
                return SavedElementBindings {
                    bindings: saved_bindings,
                    events: saved_events,
                    hovered_cell: None,
                    text_cell: None,
                };
            }
        };
        let obj = match elem_obj {
            Some(o) => o,
            None => {
                return SavedElementBindings {
                    bindings: saved_bindings,
                    events: saved_events,
                    hovered_cell: None,
                    text_cell: None,
                };
            }
        };

        // Isolate this Element call from any outer `element.*` bindings.
        // Only clear when the element declares its own `element:` argument.
        // Without this, nested elements that declare `hovered: LINK`
        // can accidentally inherit the parent `element.hovered` cell,
        // wiring hover handlers to the wrong element (e.g. Todo row label
        // toggling the row's hover state and hiding the delete button).
        for k in keys {
            self.name_to_cell.remove(&k);
        }
        self.element_events.remove("element");

        // Allocate a namespace cell for "element".
        let element_cell = self.alloc_cell("element", span);
        self.name_to_cell
            .insert("element".to_string(), element_cell);
        self.nodes.push(IrNode::Derived {
            cell: element_cell,
            expr: IrExpr::Constant(IrValue::Void),
        });

        let element_name = "element".to_string();

        for field in &obj.variables {
            let field_name = field.node.name.as_str();
            match field_name {
                "event" => {
                    // event: [press: LINK, key_down: LINK, change: LINK, ...]
                    if let Expression::Object(event_obj) = &field.node.value.node {
                        // Check if pre-allocated events exist for the LINK target.
                        // When current_var_name is set (LINK connection), reuse
                        // pre-allocated EventIds so the bridge and event consumers
                        // (HOLD body, THEN, etc.) use the same EventId.
                        let prealloc_events = self
                            .current_var_name
                            .as_ref()
                            .and_then(|name| self.element_events.get(name))
                            .cloned();
                        let mut events = HashMap::new();
                        for event_field in &event_obj.variables {
                            if matches!(event_field.node.value.node, Expression::Link) {
                                let event_name = event_field.node.name.as_str().to_string();
                                // Reuse pre-allocated EventId if available.
                                let event_id = prealloc_events
                                    .as_ref()
                                    .and_then(|pe| pe.get(&event_name).copied())
                                    .unwrap_or_else(|| {
                                        self.alloc_event(
                                            &format!("element.{}", event_name),
                                            EventSource::Link {
                                                element: element_cell,
                                                event_name: event_name.clone(),
                                            },
                                            span,
                                        )
                                    });
                                events.insert(event_name.clone(), event_id);

                                // Pre-allocate payload cells for known event types.
                                if event_name == "key_down" {
                                    let key_cell_name = format!("element.event.key_down.key");
                                    let key_cell = self.alloc_cell(&key_cell_name, span);
                                    self.name_to_cell.insert(key_cell_name, key_cell);
                                    self.nodes.push(IrNode::Derived {
                                        cell: key_cell,
                                        expr: IrExpr::Constant(IrValue::Void),
                                    });
                                    self.cell_events.insert(key_cell, event_id);
                                    self.events[event_id.0 as usize]
                                        .payload_cells
                                        .push(key_cell);
                                }
                                if event_name == "change" {
                                    let text_name = format!("element.event.change.text");
                                    let text_cell = self.alloc_cell(&text_name, span);
                                    self.name_to_cell.insert(text_name, text_cell);
                                    self.nodes.push(IrNode::Derived {
                                        cell: text_cell,
                                        expr: IrExpr::Constant(IrValue::Void),
                                    });
                                    let value_name = format!("element.event.change.value");
                                    let value_cell = self.alloc_cell(&value_name, span);
                                    self.name_to_cell.insert(value_name, value_cell);
                                    self.nodes.push(IrNode::Derived {
                                        cell: value_cell,
                                        expr: IrExpr::Constant(IrValue::Void),
                                    });
                                    self.cell_events.insert(text_cell, event_id);
                                    self.cell_events.insert(value_cell, event_id);
                                    self.events[event_id.0 as usize]
                                        .payload_cells
                                        .push(text_cell);
                                    self.events[event_id.0 as usize]
                                        .payload_cells
                                        .push(value_cell);
                                }
                                if event_name == "blur"
                                    || event_name == "focus"
                                    || event_name == "click"
                                    || event_name == "double_click"
                                {
                                    // Simple events — no payload cells needed,
                                    // but register a cell for the event source.
                                    let evt_cell_name = format!("element.event.{}", event_name);
                                    let evt_cell = self.alloc_cell(&evt_cell_name, span);
                                    self.name_to_cell.insert(evt_cell_name, evt_cell);
                                    self.nodes.push(IrNode::Derived {
                                        cell: evt_cell,
                                        expr: IrExpr::Constant(IrValue::Void),
                                    });
                                    self.cell_events.insert(evt_cell, event_id);
                                }
                            }
                        }
                        self.element_events
                            .insert(element_name.clone(), events.clone());
                        // Also store events under current_var_name (LINK target)
                        // so they survive restore_element_self_ref cleanup.
                        // Without this, the LINK processing in lower_pipe can't find
                        // events under "element" because restore already removed them.
                        if let Some(ref var_name) = self.current_var_name {
                            self.element_events.insert(var_name.clone(), events);
                        }
                    }
                }
                "hovered" => {
                    if matches!(field.node.value.node, Expression::Link) {
                        // element.hovered → CellId (boolean, true when hovered)
                        let hovered_name = "element.hovered".to_string();
                        let hovered_cell = self.alloc_cell(&hovered_name, span);
                        self.name_to_cell.insert(hovered_name, hovered_cell);
                        self.nodes.push(IrNode::Derived {
                            cell: hovered_cell,
                            expr: IrExpr::Constant(IrValue::Void),
                        });
                    }
                }
                "tag" => {
                    // tag: Header, H1, etc. — just informational, no cell needed.
                }
                _ => {
                    // Other fields — register as cells if LINK.
                    if matches!(field.node.value.node, Expression::Link) {
                        let prop_name = format!("element.{}", field_name);
                        let prop_cell = self.alloc_cell(&prop_name, span);
                        self.name_to_cell.insert(prop_name, prop_cell);
                        self.nodes.push(IrNode::Derived {
                            cell: prop_cell,
                            expr: IrExpr::Constant(IrValue::Void),
                        });
                    }
                }
            }
        }

        // Also register .text for the element (for text inputs).
        let text_name = "element.text".to_string();
        if self.name_to_cell.get(&text_name).is_none() {
            let text_cell = self.alloc_cell(&text_name, span);
            self.name_to_cell.insert(text_name, text_cell);
            self.nodes.push(IrNode::Derived {
                cell: text_cell,
                expr: IrExpr::Constant(IrValue::Void),
            });
        }

        // Only return hovered_cell if THIS element declared its own `hovered: LINK`.
        let hovered_cell = self.name_to_cell.get("element.hovered").copied();
        let text_cell = self.name_to_cell.get("element.text").copied();

        SavedElementBindings {
            bindings: saved_bindings,
            events: saved_events,
            hovered_cell,
            text_cell,
        }
    }

    /// Restore element bindings after processing an Element call.
    fn restore_element_self_ref(&mut self, saved: SavedElementBindings) {
        // Remove ALL current element.* names (created by this Element call).
        let to_remove: Vec<String> = self
            .name_to_cell
            .keys()
            .filter(|k| *k == "element" || k.starts_with("element."))
            .cloned()
            .collect();
        for k in to_remove {
            self.name_to_cell.remove(&k);
        }
        // Restore previously saved bindings.
        for (name, cell) in saved.bindings {
            if let Some(c) = cell {
                self.name_to_cell.insert(name, c);
            }
        }
        // Restore saved element_events.
        if let Some(events) = saved.events {
            self.element_events.insert("element".to_string(), events);
        } else {
            self.element_events.remove("element");
        }
    }

    /// Check if a top-level argument is present with value True (e.g. `focus: True`).
    fn has_top_level_bool_arg(&self, arguments: &[Spanned<Argument>], arg_name: &str) -> bool {
        arguments.iter().any(|a| {
            a.node.name.as_str() == arg_name
                && a.node
                    .value
                    .as_ref()
                    .map_or(false, |v| Self::is_true_expr(&v.node))
        })
    }

    fn has_element_bool_field(&self, arguments: &[Spanned<Argument>], field_name: &str) -> bool {
        let elem_arg = arguments.iter().find(|a| a.node.name.as_str() == "element");
        let elem_obj = match elem_arg {
            Some(arg) => match arg.node.value.as_ref() {
                Some(val) => match &val.node {
                    Expression::Object(obj) => Some(obj),
                    _ => None,
                },
                None => None,
            },
            None => return false,
        };
        if let Some(obj) = elem_obj {
            for field in &obj.variables {
                if field.node.name.as_str() == field_name {
                    if Self::is_true_expr(&field.node.value.node) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Check if an expression represents the literal `True` value.
    /// The parser emits `True` as `Literal::Tag("True")`, not as a Variable.
    fn is_true_expr(expr: &Expression) -> bool {
        matches!(expr, Expression::Literal(Literal::Tag(t)) if t.as_str() == "True")
    }

    /// Lower a pipe chain: `from |> to`.
    ///
    /// Strategy: always lower `from` into a cell first, then dispatch to
    /// `lower_pipe_with_source_cell`. This ensures each sub-expression is
    /// lowered exactly once (no duplicate nodes from re-lowering complex `from`).
    fn lower_pipe(
        &mut self,
        target: CellId,
        from: &Spanned<Expression>,
        to: &Spanned<Expression>,
        var_span: Span,
    ) {
        // Special case: HOLD needs the from as an *expression* for init, not a cell.
        if let Expression::Hold { state_param, body } = &to.node {
            let init_expr = self.lower_expr(&from.node, from.span);
            let state_name = state_param.as_str().to_string();

            // Try to lower as HoldLoop (object HOLD with Stream/pulses).
            if let Some(hold_loop) = self.try_lower_hold_loop(
                &state_name,
                target,
                &init_expr,
                &body.node,
                body.span,
                from.span,
            ) {
                self.nodes.push(hold_loop);
                return;
            }

            // Regular HOLD: bind state parameter name to the HOLD cell.
            let saved = self.name_to_cell.get(&state_name).copied();
            self.name_to_cell.insert(state_name.clone(), target);
            let trigger_bodies = self.lower_hold_body(&body.node, body.span, to.span);
            // Restore previous binding.
            if let Some(prev) = saved {
                self.name_to_cell.insert(state_name, prev);
            } else {
                self.name_to_cell.remove(&state_name);
            }
            self.propagate_expr_field_cells(target, &init_expr);
            for (_trigger, body) in &trigger_bodies {
                self.propagate_expr_field_cells(target, body);
            }
            self.nodes.push(IrNode::Hold {
                cell: target,
                init: init_expr,
                trigger_bodies,
            });
            return;
        }

        // Special case: Timer/interval needs Duration → ms conversion on the
        // original from expression (before it gets lowered to a cell).
        if let Expression::FunctionCall { path, .. } = &to.node {
            let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
            if path_strs.as_slice() == ["Timer", "interval"] {
                let duration = self.lower_duration_to_ms(&from.node, from.span);
                let event = self.alloc_event("timer", EventSource::Timer, to.span);
                self.nodes.push(IrNode::Timer {
                    event,
                    interval_ms: duration,
                });
                self.nodes.push(IrNode::Derived {
                    cell: target,
                    expr: IrExpr::Constant(IrValue::Void),
                });
                self.cell_events.insert(target, event);
                return;
            }
        }

        if let Expression::Then { body } = &to.node {
            let source_cell = self.lower_expr_to_cell(from, "pipe_from");
            if let Some(trigger) = self
                .resolve_event_from_expr(&from.node)
                .or_else(|| self.resolve_event_from_cell(source_cell))
            {
                let body_expr = self.lower_expr(&body.node, body.span);
                self.propagate_then_result_metadata(target, &body_expr, body.span);
                self.nodes.push(IrNode::Then {
                    cell: target,
                    trigger,
                    body: body_expr,
                });
                self.cell_events.insert(target, trigger);
                return;
            }
            if self.try_lower_then_from_latest_source(target, source_cell, body, body.span) {
                return;
            }
            self.errors.push(CompileError {
                span: to.span,
                message: format!(
                    "expected a concrete event source for THEN, but the original source does not resolve to an event ({})",
                    self.debug_event_resolution(&from.node)
                ),
            });
            self.nodes.push(IrNode::Derived {
                cell: target,
                expr: IrExpr::Constant(IrValue::Void),
            });
            return;
        }

        // Special case: LinkSetter needs to set current_var_name BEFORE
        // lowering `from`, so the element picks up the LINK target's events.
        if let Expression::LinkSetter { alias } = &to.node {
            let target_name = self.resolve_link_target_name(&alias.node, alias.span);
            if let Some(ref name) = target_name {
                let saved_var_name = self.current_var_name.take();
                self.current_var_name = Some(name.clone());
                let source_cell = self.lower_expr_to_cell(from, "pipe_from");
                self.rebind_element_links_to_target(source_cell, name);
                // Note: event propagation to LINK target name is handled by
                // process_element_self_ref (which stores events under current_var_name).
                // Do NOT copy from "element" here — restore_element_self_ref has
                // already run, so "element" holds the OUTER element's events.
                self.nodes.push(IrNode::PipeThrough {
                    cell: target,
                    source: source_cell,
                });
                self.current_var_name = saved_var_name;
                return;
            }
        }

        // Special case: nested pipe `from |> mid |> final` where final is LinkSetter.
        // We need to detect `from |> LINK { target }` anywhere in the chain.
        if let Expression::Pipe {
            from: mid,
            to: final_to,
        } = &to.node
        {
            if let Expression::LinkSetter { alias } = &final_to.node {
                let target_name = self.resolve_link_target_name(&alias.node, alias.span);
                if let Some(ref name) = target_name {
                    let saved_var_name = self.current_var_name.take();
                    self.current_var_name = Some(name.clone());
                    // Lower from |> mid as a whole, then pipe through.
                    let source_cell = self.lower_expr_to_cell(from, "pipe_from");
                    let mid_cell = self.alloc_cell("pipe_mid", to.span);
                    self.lower_pipe_with_source_cell(mid_cell, source_cell, mid, var_span);
                    self.rebind_element_links_to_target(mid_cell, name);
                    // Note: event propagation handled by process_element_self_ref.
                    self.nodes.push(IrNode::PipeThrough {
                        cell: target,
                        source: mid_cell,
                    });
                    self.current_var_name = saved_var_name;
                    return;
                }
            }
        }

        // Lower `from` into a cell, then dispatch on `to`.
        let source_cell = self.lower_expr_to_cell(from, "pipe_from");
        self.lower_pipe_with_source_cell(target, source_cell, to, var_span);
    }

    /// Lower a pipe into a function call: `source |> FunctionCall(args)`.
    fn lower_pipe_to_function_call(
        &mut self,
        target: CellId,
        source: &Spanned<Expression>,
        path: &[crate::parser::StrSlice],
        arguments: &[Spanned<Argument>],
        call_span: Span,
        var_span: Span,
    ) {
        let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();

        // Remap Scene/Element/* → Element/* (Scene elements are just Element aliases)
        let remapped: Vec<&str>;
        let effective =
            if path_strs.len() == 3 && path_strs[0] == "Scene" && path_strs[1] == "Element" {
                remapped = vec!["Element", path_strs[2]];
                &remapped
            } else {
                &path_strs
            };

        match effective.as_slice() {
            // --- `source |> Math/sum()` ---
            ["Math", "sum"] => {
                let input_cell = self.lower_expr_to_cell(source, "sum_input");
                self.nodes.push(IrNode::MathSum {
                    cell: target,
                    input: input_cell,
                });
                // Propagate event source through MathSum.
                if let Some(event) = self.cell_events.get(&input_cell).copied() {
                    self.cell_events.insert(target, event);
                }
            }

            // --- `source |> Document/new(...)` ---
            ["Document", "new"] => {
                // Also check for a `root:` argument.
                let root = self.find_arg_cell(arguments, "root", source, call_span);
                self.set_render_root(RenderSurface::Document, root, None, None);
                // The target cell = the document root.
                self.nodes.push(IrNode::PipeThrough {
                    cell: target,
                    source: root,
                });
            }

            // --- `source |> Timer/interval()` ---
            ["Timer", "interval"] => {
                // In pipe context, the source IS the duration.
                let duration = self.lower_duration_to_ms(&source.node, source.span);
                let event = self.alloc_event("timer", EventSource::Timer, call_span);
                self.nodes.push(IrNode::Timer {
                    event,
                    interval_ms: duration,
                });
                self.nodes.push(IrNode::Derived {
                    cell: target,
                    expr: IrExpr::Constant(IrValue::Void),
                });
                // Register event source for downstream THEN nodes.
                self.cell_events.insert(target, event);
            }

            // --- `source |> List/append(item: ..., on: ...)` ---
            ["List", "append"] => {
                let source_cell = self.lower_expr_to_cell(source, "list_source");
                let item_arg = arguments.iter().find(|a| a.node.name.as_str() == "item");
                if let Some(arg) = item_arg {
                    if let Some(ref val) = arg.node.value {
                        // Extract constructor from item argument (e.g., `title |> new_todo()`).
                        // This is needed when the source list is empty (LIST {}).
                        let item_constructor = self.extract_constructor_from_expr(&val.node);
                        // Find reactive dependency BEFORE lowering (lowering may consume names).
                        let reactive_dep = self.find_reactive_cell_in_expr(&val.node);
                        let item_cell = self.lower_expr_to_cell(val, "append_item");
                        // Check explicit `on` argument first, then try item cell/expr.
                        let on_arg = arguments.iter().find(|a| a.node.name.as_str() == "on");
                        let trigger = on_arg
                            .and_then(|a| a.node.value.as_ref())
                            .and_then(|v| self.resolve_event_from_expr(&v.node))
                            .or_else(|| self.resolve_event_from_cell(item_cell))
                            .or_else(|| self.resolve_event_from_expr(&val.node))
                            .unwrap_or_else(|| {
                                self.alloc_event(
                                    "list_append_trigger",
                                    EventSource::Synthetic,
                                    call_span,
                                )
                            });
                        // If trigger is Synthetic and we found a reactive dependency,
                        // use it as watch_cell so downstream updates trigger the append.
                        let watch_cell = if matches!(
                            self.events[trigger.0 as usize].source,
                            EventSource::Synthetic
                        ) {
                            reactive_dep
                        } else {
                            None
                        };
                        self.nodes.push(IrNode::ListAppend {
                            cell: target,
                            source: source_cell,
                            item: item_cell,
                            trigger,
                            watch_cell,
                        });
                        // Register constructor from item arg (takes priority),
                        // then propagate from source as fallback.
                        if let Some(ctor) = item_constructor {
                            self.list_item_constructor.insert(target, ctor);
                        } else {
                            self.propagate_list_constructor(source_cell, target);
                        }
                        return;
                    }
                }
                // Fallback: pass-through.
                self.nodes.push(IrNode::PipeThrough {
                    cell: target,
                    source: source_cell,
                });
                self.propagate_list_constructor(source_cell, target);
            }

            // --- `source |> List/clear(on: event)` ---
            ["List", "clear"] => {
                let source_cell = self.lower_expr_to_cell(source, "list_source");
                let on_arg = arguments.iter().find(|a| a.node.name.as_str() == "on");
                if let Some(arg) = on_arg {
                    if let Some(ref val) = arg.node.value {
                        let trigger = self
                            .resolve_event_from_expr(&val.node)
                            .or_else(|| {
                                let trigger_cell = self.lower_expr_to_cell(val, "clear_trigger");
                                self.resolve_event_from_cell(trigger_cell)
                            })
                            .unwrap_or_else(|| {
                                self.alloc_event(
                                    "list_clear_trigger",
                                    EventSource::Synthetic,
                                    call_span,
                                )
                            });
                        self.nodes.push(IrNode::ListClear {
                            cell: target,
                            source: source_cell,
                            trigger,
                        });
                        self.propagate_list_constructor(source_cell, target);
                        return;
                    }
                }
                self.nodes.push(IrNode::PipeThrough {
                    cell: target,
                    source: source_cell,
                });
                self.propagate_list_constructor(source_cell, target);
            }

            // --- `source |> List/count()` ---
            ["List", "count"] => {
                let source_cell = self.lower_expr_to_cell(source, "list_source");
                self.nodes.push(IrNode::ListCount {
                    cell: target,
                    source: source_cell,
                });
            }

            // --- `source |> List/map(item, to: template)` ---
            ["List", "map"] => {
                let source_cell = self.lower_expr_to_cell(source, "list_source");
                // Find the item parameter name (first positional arg or "item").
                let item_name = arguments
                    .first()
                    .map(|a| a.node.name.as_str().to_string())
                    .unwrap_or_else(|| "item".to_string());
                // Find the template (the "to" or "new" argument).
                let template_arg = arguments
                    .iter()
                    .find(|a| matches!(a.node.name.as_str(), "to" | "new"));
                if let Some(arg) = template_arg {
                    if let Some(ref val) = arg.node.value {
                        let template_constructor = self.extract_constructor_from_expr(&val.node);
                        // Record range start before template lowering.
                        let cell_start = self.cells.len() as u32;
                        let event_start = self.events.len() as u32;
                        // Create a placeholder cell for the item binding.
                        let item_cell = self.alloc_cell(&item_name, call_span);
                        self.name_to_cell.insert(item_name.clone(), item_cell);
                        // Inline the list item constructor to create per-item cells
                        // (HOLDs, LINKs, events) in the template range. This enables
                        // field access like `item.editing` to resolve to actual
                        // reactive cells instead of void placeholders.
                        self.inline_list_constructor_for_template(
                            source_cell,
                            item_cell,
                            &item_name,
                            call_span,
                        );
                        let _ = self.materialize_list_item_field_cells(
                            item_cell,
                            source_cell,
                            call_span,
                        );
                        // Lower the template expression.
                        let template_expr = self.lower_expr(&val.node, val.span);
                        let template_item_fields =
                            self.extract_item_field_exprs_from_template_expr(&template_expr);
                        // Wrap in a Derived node for the template.
                        let template_cell = self.alloc_cell("list_map_template", call_span);
                        let template_node = IrNode::Derived {
                            cell: template_cell,
                            expr: template_expr,
                        };
                        // Resolve any deferred per-item removes for this list.
                        // Template-scoped events from LINK propagation are now available.
                        if let Some(removes) = self.pending_per_item_removes.remove(&source_cell) {
                            for remove in removes {
                                // Temporarily bind the remove's item_name to the map's item cell
                                // so resolve_event_from_expr can follow through field cells.
                                let saved = self.name_to_cell.get(&remove.item_name).copied();
                                let bind_cell = self
                                    .name_to_cell
                                    .get(&item_name)
                                    .copied()
                                    .unwrap_or(item_cell);
                                self.name_to_cell
                                    .insert(remove.item_name.clone(), bind_cell);
                                if let Some(event_id) =
                                    self.resolve_event_from_expr(&remove.on_expr.node)
                                {
                                    // Patch the placeholder trigger in the existing ListRemove node.
                                    if let IrNode::ListRemove { trigger, .. } =
                                        &mut self.nodes[remove.node_index]
                                    {
                                        *trigger = event_id;
                                    }
                                }
                                // Restore binding.
                                if let Some(s) = saved {
                                    self.name_to_cell.insert(remove.item_name.clone(), s);
                                } else {
                                    self.name_to_cell.remove(&remove.item_name);
                                }
                            }
                        }
                        let cell_end = self.cells.len() as u32;
                        let event_end = self.events.len() as u32;
                        self.nodes.push(IrNode::ListMap {
                            cell: target,
                            source: source_cell,
                            item_name,
                            item_cell,
                            template: Box::new(template_node),
                            template_cell_range: (cell_start, cell_end),
                            template_event_range: (event_start, event_end),
                        });
                        if let Some(constructor) = template_constructor {
                            self.list_item_constructor.insert(target, constructor);
                        }
                        if let Some(fields) = template_item_fields {
                            self.list_item_field_exprs.insert(target, fields);
                        }
                        return;
                    }
                }
                self.nodes.push(IrNode::PipeThrough {
                    cell: target,
                    source: source_cell,
                });
            }
            ["List", "latest"] => {
                let source_cell = self.lower_expr_to_cell(source, "list_source");
                if let Some(arms) = self.lower_list_latest_from_source(source_cell) {
                    for arm in &arms {
                        self.propagate_expr_field_cells(target, &arm.body);
                    }
                    self.nodes.push(IrNode::Latest { target, arms });
                } else {
                    self.nodes.push(IrNode::PipeThrough {
                        cell: target,
                        source: source_cell,
                    });
                }
            }

            // --- `source |> Stream/pulses()` ---
            ["Stream", "pulses"] => {
                // Stream/pulses converts a number into N events.
                let source_cell = self.lower_expr_to_cell(source, "pulses_source");
                self.nodes.push(IrNode::PipeThrough {
                    cell: target,
                    source: source_cell,
                });
            }

            // --- `source |> Stream/skip(count: N)` ---
            ["Stream", "skip"] => {
                let source_cell = self.lower_expr_to_cell(source, "skip_source");
                let skip_count = arguments
                    .iter()
                    .find(|arg| arg.node.name.as_str() == "count")
                    .and_then(|arg| arg.node.value.as_ref())
                    .and_then(|value| match &value.node {
                        Expression::Literal(crate::parser::static_expression::Literal::Number(
                            n,
                        )) if *n >= 0.0 => Some(*n as usize),
                        _ => None,
                    })
                    .unwrap_or(0);
                let seen_cell = self.alloc_cell("stream_skip_seen", call_span);
                self.nodes.push(IrNode::Derived {
                    cell: seen_cell,
                    expr: IrExpr::Constant(IrValue::Number(0.0)),
                });
                self.nodes.push(IrNode::StreamSkip {
                    cell: target,
                    source: source_cell,
                    count: skip_count,
                    seen_cell,
                });
                if let Some(fields) = self.cell_field_cells.get(&source_cell).cloned() {
                    self.cell_field_cells.insert(target, fields);
                }
            }

            // --- `source |> Text/trim()` ---
            ["Text", "trim"] => {
                let source_cell = self.lower_expr_to_cell(source, "trim_source");
                self.nodes.push(IrNode::TextTrim {
                    cell: target,
                    source: source_cell,
                });
            }

            // --- `source |> Text/to_number()` ---
            ["Text", "to_number"] => {
                let source_cell = self.lower_expr_to_cell(source, "to_number_source");
                let nan_tag_value = self.intern_tag("NaN");
                self.nodes.push(IrNode::TextToNumber {
                    cell: target,
                    source: source_cell,
                    nan_tag_value,
                });
            }

            // --- `source |> Math/round()` ---
            ["Math", "round"] => {
                let source_cell = self.lower_expr_to_cell(source, "round_source");
                self.nodes.push(IrNode::MathRound {
                    cell: target,
                    source: source_cell,
                });
            }

            // --- `source |> Math/min(b: ...)` ---
            ["Math", "min"] => {
                let source_cell = self.lower_expr_to_cell(source, "min_source");
                let b_cell =
                    if let Some(b_arg) = arguments.iter().find(|a| a.node.name.as_str() == "b") {
                        if let Some(ref val) = b_arg.node.value {
                            self.lower_expr_to_cell(val, "min_b")
                        } else {
                            self.alloc_cell("min_b_empty", call_span)
                        }
                    } else {
                        self.alloc_cell("min_b_empty", call_span)
                    };
                self.nodes.push(IrNode::MathMin {
                    cell: target,
                    source: source_cell,
                    b: b_cell,
                });
            }

            // --- `source |> Math/max(b: ...)` ---
            ["Math", "max"] => {
                let source_cell = self.lower_expr_to_cell(source, "max_source");
                let b_cell =
                    if let Some(b_arg) = arguments.iter().find(|a| a.node.name.as_str() == "b") {
                        if let Some(ref val) = b_arg.node.value {
                            self.lower_expr_to_cell(val, "max_b")
                        } else {
                            self.alloc_cell("max_b_empty", call_span)
                        }
                    } else {
                        self.alloc_cell("max_b_empty", call_span)
                    };
                self.nodes.push(IrNode::MathMax {
                    cell: target,
                    source: source_cell,
                    b: b_cell,
                });
            }

            // --- `source |> Text/starts_with(prefix: ...)` ---
            ["Text", "starts_with"] => {
                let source_cell = self.lower_expr_to_cell(source, "starts_with_source");
                let prefix_cell = if let Some(prefix_arg) =
                    arguments.iter().find(|a| a.node.name.as_str() == "prefix")
                {
                    if let Some(ref val) = prefix_arg.node.value {
                        self.lower_expr_to_cell(val, "starts_with_prefix")
                    } else {
                        self.alloc_cell("starts_with_prefix_empty", call_span)
                    }
                } else {
                    self.alloc_cell("starts_with_prefix_empty", call_span)
                };
                self.nodes.push(IrNode::TextStartsWith {
                    cell: target,
                    source: source_cell,
                    prefix: prefix_cell,
                });
            }

            // --- `source |> Text/length()` ---
            ["Text", "length"] => {
                let source_cell = self.lower_expr_to_cell(source, "text_length_source");
                self.nodes.push(IrNode::CustomCall {
                    cell: target,
                    path: vec!["Text".to_string(), "length".to_string()],
                    args: vec![("__source".to_string(), IrExpr::CellRead(source_cell))],
                });
            }

            // --- `source |> Text/find(search: ...)` ---
            ["Text", "find"] => {
                let source_cell = self.lower_expr_to_cell(source, "text_find_source");
                let search_cell = if let Some(search_arg) =
                    arguments.iter().find(|a| a.node.name.as_str() == "search")
                {
                    if let Some(ref val) = search_arg.node.value {
                        self.lower_expr_to_cell(val, "text_find_search")
                    } else {
                        self.alloc_cell("text_find_search_empty", call_span)
                    }
                } else {
                    self.alloc_cell("text_find_search_empty", call_span)
                };
                self.nodes.push(IrNode::CustomCall {
                    cell: target,
                    path: vec!["Text".to_string(), "find".to_string()],
                    args: vec![
                        ("__source".to_string(), IrExpr::CellRead(source_cell)),
                        ("search".to_string(), IrExpr::CellRead(search_cell)),
                    ],
                });
            }

            // --- `source |> Text/substring(start: ..., length: ...)` ---
            ["Text", "substring"] => {
                let source_cell = self.lower_expr_to_cell(source, "text_substring_source");
                let start_cell = if let Some(start_arg) =
                    arguments.iter().find(|a| a.node.name.as_str() == "start")
                {
                    if let Some(ref val) = start_arg.node.value {
                        self.lower_expr_to_cell(val, "text_substring_start")
                    } else {
                        self.alloc_cell("text_substring_start_empty", call_span)
                    }
                } else {
                    self.alloc_cell("text_substring_start_empty", call_span)
                };
                let length_cell = if let Some(length_arg) =
                    arguments.iter().find(|a| a.node.name.as_str() == "length")
                {
                    if let Some(ref val) = length_arg.node.value {
                        self.lower_expr_to_cell(val, "text_substring_length")
                    } else {
                        self.alloc_cell("text_substring_length_empty", call_span)
                    }
                } else {
                    self.alloc_cell("text_substring_length_empty", call_span)
                };
                self.nodes.push(IrNode::CustomCall {
                    cell: target,
                    path: vec!["Text".to_string(), "substring".to_string()],
                    args: vec![
                        ("__source".to_string(), IrExpr::CellRead(source_cell)),
                        ("start".to_string(), IrExpr::CellRead(start_cell)),
                        ("length".to_string(), IrExpr::CellRead(length_cell)),
                    ],
                });
            }

            // --- `source |> Text/is_empty()` ---
            ["Text", "is_empty"] => {
                let source_cell = self.lower_expr_to_cell(source, "is_empty_source");
                self.nodes.push(IrNode::CustomCall {
                    cell: target,
                    path: vec!["Text".to_string(), "is_empty".to_string()],
                    args: vec![("__source".to_string(), IrExpr::CellRead(source_cell))],
                });
            }

            // --- `source |> Text/is_not_empty()` ---
            ["Text", "is_not_empty"] => {
                let source_cell = self.lower_expr_to_cell(source, "is_not_empty_source");
                self.nodes.push(IrNode::TextIsNotEmpty {
                    cell: target,
                    source: source_cell,
                });
            }

            // --- `source |> Bool/not()` ---
            ["Bool", "not"] => {
                let source_cell = self.lower_expr_to_cell(source, "bool_not_source");
                self.nodes.push(IrNode::Derived {
                    cell: target,
                    expr: IrExpr::Not(Box::new(IrExpr::CellRead(source_cell))),
                });
            }

            // --- `source |> Bool/toggle(when: event)` ---
            // Equivalent to: source |> HOLD state { event |> THEN { state |> Bool/not() } }
            ["Bool", "toggle"] => {
                let source_cell = self.lower_expr_to_cell(source, "toggle_source");
                let when_arg = arguments.iter().find(|a| a.node.name.as_str() == "when");
                if let Some(when_arg) = when_arg {
                    if let Some(ref val) = when_arg.node.value {
                        // Try event resolution from expression FIRST (handles
                        // element.event.click paths), then fall back to cell-based.
                        let trigger =
                            self.resolve_event_from_expr(&val.node).unwrap_or_else(|| {
                                let when_cell = self.lower_expr_to_cell(val, "toggle_when");
                                self.resolve_event_from_cell(when_cell).unwrap_or_else(|| {
                                    self.alloc_event(
                                        "toggle_trigger",
                                        EventSource::Synthetic,
                                        call_span,
                                    )
                                })
                            });
                        self.nodes.push(IrNode::Hold {
                            cell: target,
                            init: IrExpr::CellRead(source_cell),
                            trigger_bodies: vec![(
                                trigger,
                                IrExpr::Not(Box::new(IrExpr::CellRead(target))),
                            )],
                        });
                    } else {
                        self.nodes.push(IrNode::PipeThrough {
                            cell: target,
                            source: source_cell,
                        });
                    }
                } else {
                    self.nodes.push(IrNode::PipeThrough {
                        cell: target,
                        source: source_cell,
                    });
                }
            }

            // --- `source |> List/is_empty()` ---
            ["List", "is_empty"] => {
                let source_cell = self.lower_expr_to_cell(source, "list_is_empty_source");
                self.nodes.push(IrNode::ListIsEmpty {
                    cell: target,
                    source: source_cell,
                });
            }

            // --- `source |> List/is_not_empty()` ---
            ["List", "is_not_empty"] => {
                let source_cell = self.lower_expr_to_cell(source, "list_is_not_empty_source");
                let is_empty_cell = self.alloc_cell("list_is_empty_intermediate", call_span);
                self.nodes.push(IrNode::ListIsEmpty {
                    cell: is_empty_cell,
                    source: source_cell,
                });
                self.nodes.push(IrNode::Derived {
                    cell: target,
                    expr: IrExpr::Not(Box::new(IrExpr::CellRead(is_empty_cell))),
                });
            }

            // --- `source |> List/remove(item, on: event)` ---
            ["List", "remove"] => {
                let source_cell = self.lower_expr_to_cell(source, "list_source");
                // Bind the item parameter (first positional arg) so the `on:` expression
                // can reference item fields (e.g., item.todo_elements.remove_button.event.press).
                let item_name = arguments
                    .first()
                    .filter(|a| a.node.name.as_str() != "on" && a.node.name.as_str() != "if")
                    .map(|a| a.node.name.as_str().to_string())
                    .unwrap_or_else(|| "item".to_string());
                let saved_item = self.name_to_cell.get(&item_name).copied();
                let item_cell = self.alloc_cell(&item_name, call_span);
                self.name_to_cell.insert(item_name.clone(), item_cell);
                self.nodes.push(IrNode::Derived {
                    cell: item_cell,
                    expr: IrExpr::Constant(IrValue::Void),
                });

                let on_arg = arguments.iter().find(|a| a.node.name.as_str() == "on");
                if let Some(arg) = on_arg {
                    if let Some(ref val) = arg.node.value {
                        // Case 2: Pipe { from: <event>, to: Then { body } } with per-item predicate.
                        // Detect: on: global_event |> THEN { item.field |> WHEN { ... } }
                        if let Expression::Pipe { from, to } = &val.node {
                            if let Expression::Then { body } = &to.node {
                                if let Some(event_id) =
                                    self.resolve_event_from_expr(&from.node).or_else(|| {
                                        let c = self.lower_expr_to_cell(from, "remove_event");
                                        self.resolve_event_from_cell(c)
                                    })
                                {
                                    // Pre-scan THEN body for item field references.
                                    let field_names = collect_item_field_names(body, &item_name);
                                    let mut item_field_cells = Vec::new();
                                    if !field_names.is_empty() {
                                        let mut field_map = HashMap::new();
                                        for field in &field_names {
                                            let sub_cell = self.alloc_cell(
                                                &format!("{}.{}", item_name, field),
                                                call_span,
                                            );
                                            self.nodes.push(IrNode::Derived {
                                                cell: sub_cell,
                                                expr: IrExpr::Constant(IrValue::Void),
                                            });
                                            self.name_to_cell.insert(
                                                format!("{}.{}", item_name, field),
                                                sub_cell,
                                            );
                                            field_map.insert(field.clone(), sub_cell);
                                            item_field_cells.push((field.clone(), sub_cell));
                                        }
                                        if !field_map.is_empty() {
                                            self.cell_field_cells.insert(item_cell, field_map);
                                        }
                                    }
                                    // Lower THEN body to get predicate cell.
                                    let predicate_cell = if !item_field_cells.is_empty() {
                                        Some(self.lower_expr_to_cell(body, "remove_pred"))
                                    } else {
                                        None
                                    };
                                    // Restore item binding.
                                    if let Some(prev) = saved_item {
                                        self.name_to_cell.insert(item_name.clone(), prev);
                                    } else {
                                        self.name_to_cell.remove(&item_name);
                                    }
                                    // Clean up sub-cell name_to_cell entries.
                                    for (field, _) in &item_field_cells {
                                        self.name_to_cell
                                            .remove(&format!("{}.{}", item_name, field));
                                    }
                                    self.nodes.push(IrNode::ListRemove {
                                        cell: target,
                                        source: source_cell,
                                        trigger: event_id,
                                        predicate: predicate_cell,
                                        item_cell: if item_field_cells.is_empty() {
                                            None
                                        } else {
                                            Some(item_cell)
                                        },
                                        item_field_cells,
                                    });
                                    self.propagate_list_constructor(source_cell, target);
                                    return;
                                }
                            }
                        }
                        // Case 1: Direct event (per-item or simple global).
                        let trigger = self.resolve_event_from_expr(&val.node);
                        if let Some(trigger) = trigger {
                            // Restore item binding.
                            if let Some(prev) = saved_item {
                                self.name_to_cell.insert(item_name, prev);
                            } else {
                                self.name_to_cell.remove(&item_name);
                            }
                            self.nodes.push(IrNode::ListRemove {
                                cell: target,
                                source: source_cell,
                                trigger,
                                predicate: None,
                                item_cell: None,
                                item_field_cells: vec![],
                            });
                            self.propagate_list_constructor(source_cell, target);
                            return;
                        }
                        // Check if this is a per-item event reference (alias starting with
                        // the item parameter name). If so, defer resolution to List/map where
                        // template-scoped events will be created by LINK propagation.
                        let is_per_item_event = Self::is_per_item_event_expr(&val.node, &item_name);
                        if is_per_item_event {
                            // Restore item binding.
                            if let Some(prev) = saved_item {
                                self.name_to_cell.insert(item_name.clone(), prev);
                            } else {
                                self.name_to_cell.remove(&item_name);
                            }
                            // Create ListRemove with a placeholder trigger. The trigger will
                            // be patched during List/map template lowering when the actual
                            // template-scoped event is available. Use a sentinel EventId that
                            // won't match any real event.
                            let placeholder = EventId(u32::MAX);
                            let node_index = self.nodes.len();
                            self.nodes.push(IrNode::ListRemove {
                                cell: target,
                                source: source_cell,
                                trigger: placeholder,
                                predicate: None,
                                item_cell: None,
                                item_field_cells: vec![],
                            });
                            self.pending_per_item_removes
                                .entry(source_cell)
                                .or_default()
                                .push(PendingPerItemRemove {
                                    item_name,
                                    node_index,
                                    on_expr: val.clone(),
                                });
                            self.propagate_list_constructor(source_cell, target);
                            return;
                        }
                        // Fallback: lower expression to cell and try to resolve event.
                        let trigger = {
                            let trigger_cell = self.lower_expr_to_cell(val, "remove_trigger");
                            self.resolve_event_from_cell(trigger_cell)
                        }
                        .unwrap_or_else(|| {
                            self.alloc_event(
                                "list_remove_trigger",
                                EventSource::Synthetic,
                                call_span,
                            )
                        });
                        // Restore item binding.
                        if let Some(prev) = saved_item {
                            self.name_to_cell.insert(item_name, prev);
                        } else {
                            self.name_to_cell.remove(&item_name);
                        }
                        self.nodes.push(IrNode::ListRemove {
                            cell: target,
                            source: source_cell,
                            trigger,
                            predicate: None,
                            item_cell: None,
                            item_field_cells: vec![],
                        });
                        self.propagate_list_constructor(source_cell, target);
                        return;
                    }
                }
                // Restore item binding.
                if let Some(prev) = saved_item {
                    self.name_to_cell.insert(item_name, prev);
                } else {
                    self.name_to_cell.remove(&item_name);
                }
                self.nodes.push(IrNode::PipeThrough {
                    cell: target,
                    source: source_cell,
                });
                self.propagate_list_constructor(source_cell, target);
            }

            // --- `source |> List/retain(item, if: predicate)` ---
            ["List", "retain"] => {
                let source_cell = self.lower_expr_to_cell(source, "list_source");
                // Bind the item parameter so the `if:` expression can reference item fields.
                let item_name = arguments
                    .first()
                    .filter(|a| a.node.name.as_str() != "on" && a.node.name.as_str() != "if")
                    .map(|a| a.node.name.as_str().to_string())
                    .unwrap_or_else(|| "item".to_string());
                let saved_item = self.name_to_cell.get(&item_name).copied();
                let item_cell = self.alloc_cell(&item_name, call_span);
                self.name_to_cell.insert(item_name.clone(), item_cell);
                self.nodes.push(IrNode::Derived {
                    cell: item_cell,
                    expr: IrExpr::Constant(IrValue::Void),
                });
                // Pre-scan the `if:` expression for field accesses on the item variable.
                // Create sub-cells so `item.completed` resolves to CellRead(sub_cell)
                // instead of dead FieldAccess.
                let if_expr = arguments
                    .iter()
                    .find(|a| a.node.name.as_str() == "if")
                    .and_then(|a| a.node.value.as_ref());
                let mut item_field_cells = Vec::new();
                if let Some(val) = if_expr {
                    let field_names = collect_item_field_names(val, &item_name);
                    let mut field_map = HashMap::new();
                    for field in &field_names {
                        let sub_cell =
                            self.alloc_cell(&format!("{}.{}", item_name, field), call_span);
                        self.nodes.push(IrNode::Derived {
                            cell: sub_cell,
                            expr: IrExpr::Constant(IrValue::Void),
                        });
                        self.name_to_cell
                            .insert(format!("{}.{}", item_name, field), sub_cell);
                        field_map.insert(field.clone(), sub_cell);
                        item_field_cells.push((field.clone(), sub_cell));
                    }
                    if !field_map.is_empty() {
                        self.cell_field_cells.insert(item_cell, field_map);
                    }
                }
                // Lower the `if:` predicate expression and capture it as a cell.
                let predicate_cell = if_expr.map(|val| self.lower_expr_to_cell(val, "retain_pred"));
                // Restore item binding.
                if let Some(prev) = saved_item {
                    self.name_to_cell.insert(item_name.clone(), prev);
                } else {
                    self.name_to_cell.remove(&item_name);
                }
                // Clean up sub-cell name_to_cell entries.
                for (field, _) in &item_field_cells {
                    self.name_to_cell
                        .remove(&format!("{}.{}", item_name, field));
                }
                self.nodes.push(IrNode::ListRetain {
                    cell: target,
                    source: source_cell,
                    predicate: predicate_cell,
                    item_cell: Some(item_cell),
                    item_field_cells,
                });
                self.propagate_list_constructor(source_cell, target);
            }

            // --- `source |> List/every(item, if: predicate)` ---
            // --- `source |> List/any(item, if: predicate)` ---
            ["List", "every"] | ["List", "any"] => {
                let is_every = effective[1] == "every";
                let source_cell = self.lower_expr_to_cell(source, "list_source");
                // Bind the item parameter (same pattern as List/retain).
                let item_name = arguments
                    .first()
                    .filter(|a| a.node.name.as_str() != "on" && a.node.name.as_str() != "if")
                    .map(|a| a.node.name.as_str().to_string())
                    .unwrap_or_else(|| "item".to_string());
                let saved_item = self.name_to_cell.get(&item_name).copied();
                let item_cell = self.alloc_cell(&item_name, call_span);
                self.name_to_cell.insert(item_name.clone(), item_cell);
                self.nodes.push(IrNode::Derived {
                    cell: item_cell,
                    expr: IrExpr::Constant(IrValue::Void),
                });
                // Pre-scan the `if:` expression for field accesses on the item variable.
                let if_expr = arguments
                    .iter()
                    .find(|a| a.node.name.as_str() == "if")
                    .and_then(|a| a.node.value.as_ref());
                let mut item_field_cells = Vec::new();
                if let Some(val) = if_expr {
                    let field_names = collect_item_field_names(val, &item_name);
                    let mut field_map = HashMap::new();
                    for field in &field_names {
                        let sub_cell =
                            self.alloc_cell(&format!("{}.{}", item_name, field), call_span);
                        self.nodes.push(IrNode::Derived {
                            cell: sub_cell,
                            expr: IrExpr::Constant(IrValue::Void),
                        });
                        self.name_to_cell
                            .insert(format!("{}.{}", item_name, field), sub_cell);
                        field_map.insert(field.clone(), sub_cell);
                        item_field_cells.push((field.clone(), sub_cell));
                    }
                    if !field_map.is_empty() {
                        self.cell_field_cells.insert(item_cell, field_map);
                    }
                }
                // Lower the `if:` predicate expression.
                let predicate_cell = if_expr.map(|val| self.lower_expr_to_cell(val, "check_pred"));
                // Restore item binding.
                if let Some(prev) = saved_item {
                    self.name_to_cell.insert(item_name.clone(), prev);
                } else {
                    self.name_to_cell.remove(&item_name);
                }
                for (field, _) in &item_field_cells {
                    self.name_to_cell
                        .remove(&format!("{}.{}", item_name, field));
                }
                if is_every {
                    self.nodes.push(IrNode::ListEvery {
                        cell: target,
                        source: source_cell,
                        predicate: predicate_cell,
                        item_cell: Some(item_cell),
                        item_field_cells,
                    });
                } else {
                    self.nodes.push(IrNode::ListAny {
                        cell: target,
                        source: source_cell,
                        predicate: predicate_cell,
                        item_cell: Some(item_cell),
                        item_field_cells,
                    });
                }
                // No propagate_list_constructor: these return booleans, not lists.
            }

            // --- `source |> Router/go_to()` ---
            ["Router", "go_to"] => {
                let source_cell = self.lower_expr_to_cell(source, "goto_source");
                self.nodes.push(IrNode::RouterGoTo {
                    cell: target,
                    source: source_cell,
                });
            }

            // --- User-defined function call in pipe position ---
            _ => {
                let resolved_fn = if effective.len() == 1 {
                    self.resolve_func_name(effective[0])
                } else if effective.len() == 2 {
                    let qualified = format!("{}/{}", effective[0], effective[1]);
                    self.resolve_func_name(&qualified)
                } else {
                    None
                };
                if let Some(fn_name) = resolved_fn {
                    let func_def = self.find_func_def(&fn_name).unwrap();
                    let func_id = self.name_to_func[&fn_name];
                    // Set module context for intra-module resolution during body inlining
                    self.current_module = self.function_modules.get(&fn_name).cloned();
                    let source_cell = self.lower_expr_to_cell(source, "pipe_fn_arg");
                    let result = self.inline_function_call_with_pipe(
                        &fn_name,
                        func_id,
                        &func_def,
                        source_cell,
                        arguments,
                        call_span,
                    );
                    match result {
                        IrExpr::CellRead(result_cell) => {
                            self.nodes.push(IrNode::PipeThrough {
                                cell: target,
                                source: result_cell,
                            });
                            if let Some(event) = self.cell_events.get(&result_cell).copied() {
                                self.cell_events.insert(target, event);
                            }
                            self.propagate_list_constructor(result_cell, target);
                            // Propagate concrete field cells, and force materialization
                            // when the callee preserved only list-item field metadata.
                            if let Some(fields) = self.resolve_cell_field_cells(result_cell) {
                                self.cell_field_cells.insert(target, fields);
                            } else {
                                let _ = self.materialize_list_item_field_cells(
                                    target,
                                    result_cell,
                                    call_span,
                                );
                            }
                        }
                        _ => {
                            self.nodes.push(IrNode::Derived {
                                cell: target,
                                expr: result,
                            });
                        }
                    }
                    return;
                }
                let source_cell = self.lower_expr_to_cell(source, "pipe_custom_call_source");
                let args: Vec<(String, IrExpr)> = arguments
                    .iter()
                    .map(|a| {
                        let val = a
                            .node
                            .value
                            .as_ref()
                            .map(|v| self.lower_expr(&v.node, v.span))
                            .unwrap_or(IrExpr::Constant(IrValue::Void));
                        (a.node.name.as_str().to_string(), val)
                    })
                    .chain(std::iter::once((
                        "__source".to_string(),
                        IrExpr::CellRead(source_cell),
                    )))
                    .collect();
                self.nodes.push(IrNode::CustomCall {
                    cell: target,
                    path: effective.iter().map(|s| s.to_string()).collect(),
                    args,
                });
            }
        }
    }

    /// Lower LATEST { inputs }.
    fn lower_latest(&mut self, target: CellId, inputs: &[Spanned<Expression>], var_span: Span) {
        let mut arms = Vec::new();
        for input in inputs {
            match &input.node {
                // A pipe inside LATEST — the pipe's "from" provides a trigger event.
                Expression::Pipe { from, to } => {
                    // `from |> THEN { body }` inside LATEST
                    match &to.node {
                        Expression::Then { body } => {
                            // Try to resolve 'from' as an event reference (e.g., button.event.press).
                            // If not a direct alias, lower it to a cell (may create Timer etc.)
                            let trigger = self
                                .resolve_event_from_expr(&from.node)
                                .or_else(|| {
                                    let from_cell =
                                        self.lower_expr_to_cell(from, "latest_trigger_source");
                                    self.resolve_event_from_cell(from_cell)
                                })
                                .unwrap_or_else(|| {
                                    self.alloc_event(
                                        "latest_trigger",
                                        EventSource::Synthetic,
                                        to.span,
                                    )
                                });
                            let body_expr = self.lower_expr(&body.node, body.span);
                            arms.push(LatestArm {
                                trigger: Some(trigger),
                                body: body_expr,
                            });
                        }
                        _ => {
                            // Other pipes inside LATEST.
                            let mut expr = self.lower_pipe_to_expr(from, to);
                            if let Some(resolved) = self.resolve_field_access_expr_to_cell(&expr) {
                                expr = IrExpr::CellRead(resolved);
                            }
                            if let Some(extracted) =
                                self.extract_latest_arms_from_expr(&expr, (0, u32::MAX))
                            {
                                if extracted.iter().any(|arm| arm.trigger.is_some()) {
                                    arms.extend(extracted);
                                } else {
                                    arms.push(LatestArm {
                                        trigger: None,
                                        body: expr,
                                    });
                                }
                            } else {
                                arms.push(LatestArm {
                                    trigger: None,
                                    body: expr,
                                });
                            }
                        }
                    }
                }
                _ => {
                    // Preserve triggerful lowered values (e.g. helper-returned THEN/HOLD/LATEST
                    // cells) instead of flattening them into trigger-less CellRead arms.
                    let mut expr = self.lower_expr(&input.node, input.span);
                    if let Some(resolved) = self.resolve_field_access_expr_to_cell(&expr) {
                        expr = IrExpr::CellRead(resolved);
                    }
                    if let Some(extracted) =
                        self.extract_latest_arms_from_expr(&expr, (0, u32::MAX))
                    {
                        if extracted.iter().any(|arm| arm.trigger.is_some()) {
                            arms.extend(extracted);
                        } else {
                            arms.push(LatestArm {
                                trigger: None,
                                body: expr,
                            });
                        }
                    } else {
                        arms.push(LatestArm {
                            trigger: None,
                            body: expr,
                        });
                    }
                }
            }
        }
        for arm in &arms {
            self.propagate_expr_field_cells(target, &arm.body);
        }
        self.nodes.push(IrNode::Latest { target, arms });
    }

    fn lower_list_latest_from_source(&self, source_cell: CellId) -> Option<Vec<LatestArm>> {
        let mut current = source_cell;
        for _ in 0..100 {
            let mut next_source = None;
            for node in self.nodes.iter().rev() {
                match node {
                    IrNode::ListMap {
                        cell,
                        template,
                        template_cell_range,
                        ..
                    } if *cell == current => {
                        return self.extract_latest_arms_from_template_node(
                            template,
                            *template_cell_range,
                        );
                    }
                    IrNode::PipeThrough { cell, source }
                    | IrNode::ListRetain { cell, source, .. }
                    | IrNode::ListRemove { cell, source, .. }
                        if *cell == current =>
                    {
                        next_source = Some(*source);
                        break;
                    }
                    _ => {}
                }
            }
            current = next_source?;
        }
        None
    }

    fn extract_latest_arms_from_template_node(
        &self,
        node: &IrNode,
        template_cell_range: (u32, u32),
    ) -> Option<Vec<LatestArm>> {
        match node {
            IrNode::Derived { expr, .. } => {
                self.extract_latest_arms_from_expr(expr, template_cell_range)
            }
            IrNode::Then { trigger, body, .. } => Some(vec![LatestArm {
                trigger: Some(*trigger),
                body: body.clone(),
            }]),
            IrNode::Hold { trigger_bodies, .. } => Some(
                trigger_bodies
                    .iter()
                    .map(|(trigger, body)| LatestArm {
                        trigger: Some(*trigger),
                        body: body.clone(),
                    })
                    .collect(),
            ),
            IrNode::When { source, arms, .. } | IrNode::While { source, arms, .. } => {
                self.extract_latest_arms_from_pattern_node(*source, arms)
            }
            IrNode::Latest { arms, .. } => Some(arms.clone()),
            _ => None,
        }
    }

    fn extract_latest_arms_from_pattern_node(
        &self,
        source: CellId,
        arms: &[(IrPattern, IrExpr)],
    ) -> Option<Vec<LatestArm>> {
        let trigger = match self.resolve_event_from_cell(source) {
            Some(trigger) => trigger,
            None => return None,
        };
        Some(vec![LatestArm {
            trigger: Some(trigger),
            body: IrExpr::PatternMatch {
                source,
                arms: arms.to_vec(),
            },
        }])
    }

    fn extract_latest_arms_from_expr(
        &self,
        expr: &IrExpr,
        template_cell_range: (u32, u32),
    ) -> Option<Vec<LatestArm>> {
        let mut seen = HashSet::new();
        self.extract_latest_arms_from_expr_inner(expr, template_cell_range, &mut seen)
    }

    fn extract_latest_arms_from_expr_inner(
        &self,
        expr: &IrExpr,
        template_cell_range: (u32, u32),
        seen: &mut HashSet<CellId>,
    ) -> Option<Vec<LatestArm>> {
        match expr {
            IrExpr::CellRead(cell) => {
                self.extract_latest_arms_from_cell_inner(*cell, template_cell_range, seen)
            }
            IrExpr::FieldAccess { .. } => {
                if let Some(cell) = self.resolve_field_access_expr_to_cell(expr) {
                    self.extract_latest_arms_from_cell_inner(cell, template_cell_range, seen)
                } else {
                    Some(vec![LatestArm {
                        trigger: None,
                        body: expr.clone(),
                    }])
                }
            }
            _ => Some(vec![LatestArm {
                trigger: None,
                body: expr.clone(),
            }]),
        }
    }

    fn extract_latest_arms_from_cell(
        &self,
        cell: CellId,
        template_cell_range: (u32, u32),
    ) -> Option<Vec<LatestArm>> {
        let mut seen = HashSet::new();
        self.extract_latest_arms_from_cell_inner(cell, template_cell_range, &mut seen)
    }

    fn extract_latest_arms_from_cell_inner(
        &self,
        cell: CellId,
        template_cell_range: (u32, u32),
        seen: &mut HashSet<CellId>,
    ) -> Option<Vec<LatestArm>> {
        if !seen.insert(cell) {
            return Some(vec![LatestArm {
                trigger: None,
                body: IrExpr::CellRead(cell),
            }]);
        }
        let (cell_start, cell_end) = template_cell_range;
        let node = self.nodes.iter().rev().find(|node| match node {
            IrNode::Derived {
                cell: c,
                expr: IrExpr::Constant(IrValue::Void),
            } => {
                *c == cell
                    && c.0 >= cell_start
                    && c.0 < cell_end
                    && !self.cell_field_cells.contains_key(c)
            }
            IrNode::Derived { cell: c, .. }
            | IrNode::Hold { cell: c, .. }
            | IrNode::Latest { target: c, .. }
            | IrNode::Then { cell: c, .. }
            | IrNode::When { cell: c, .. }
            | IrNode::While { cell: c, .. }
            | IrNode::TextInterpolation { cell: c, .. }
            | IrNode::MathSum { cell: c, .. }
            | IrNode::PipeThrough { cell: c, .. }
            | IrNode::StreamSkip { cell: c, .. }
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
            | IrNode::TextStartsWith { cell: c, .. }
            | IrNode::MathRound { cell: c, .. }
            | IrNode::MathMin { cell: c, .. }
            | IrNode::MathMax { cell: c, .. }
            | IrNode::HoldLoop { cell: c, .. } => *c == cell && c.0 >= cell_start && c.0 < cell_end,
            IrNode::Document { .. } | IrNode::Timer { .. } => false,
        })?;

        let result = match node {
            IrNode::Then { trigger, body, .. } => Some(vec![LatestArm {
                trigger: Some(*trigger),
                body: body.clone(),
            }]),
            IrNode::Hold { trigger_bodies, .. } => Some(
                trigger_bodies
                    .iter()
                    .map(|(trigger, body)| LatestArm {
                        trigger: Some(*trigger),
                        body: body.clone(),
                    })
                    .collect(),
            ),
            IrNode::Latest { arms, .. } => Some(arms.clone()),
            IrNode::When { source, arms, .. } | IrNode::While { source, arms, .. } => {
                self.extract_latest_arms_from_pattern_node(*source, arms)
            }
            IrNode::Derived { expr, .. } => {
                self.extract_latest_arms_from_expr_inner(expr, template_cell_range, seen)
            }
            IrNode::PipeThrough { source, .. } => {
                self.extract_latest_arms_from_cell_inner(*source, template_cell_range, seen)
            }
            _ => Some(vec![LatestArm {
                trigger: None,
                body: IrExpr::CellRead(cell),
            }]),
        };
        seen.remove(&cell);
        result
    }

    /// Lower a HOLD body expression, extracting trigger events.
    /// Common patterns:
    /// - `event_source |> THEN { expr }` → trigger from event_source, body = expr
    /// - `LATEST { arm1, arm2, ... }` → triggers from each arm's event source
    /// - plain expression → no trigger (static HOLD)
    fn lower_hold_body(
        &mut self,
        body: &Expression,
        body_span: Span,
        hold_span: Span,
    ) -> Vec<(EventId, IrExpr)> {
        match body {
            // `event_source |> THEN { expr }` — single trigger
            // Special case: `LATEST { ... } |> THEN { body }` — extract triggers
            // from each LATEST arm and pair each with the outer THEN body.
            Expression::Pipe { from, to } => {
                if let Expression::Then { body: then_body } = &to.node {
                    // Check if `from` is a LATEST expression — if so, expand into
                    // multiple trigger-body pairs (one per LATEST arm).
                    if let Expression::Latest { inputs } = &from.node {
                        let outer_body_expr = self.lower_expr(&then_body.node, then_body.span);
                        let mut trigger_bodies = Vec::new();
                        for input in inputs {
                            if let Expression::Pipe {
                                from: inner_from,
                                to: inner_to,
                            } = &input.node
                            {
                                let trigger = self
                                    .resolve_event_from_expr(&inner_from.node)
                                    .or_else(|| {
                                        let from_cell = self
                                            .lower_expr_to_cell(inner_from, "hold_trigger_source");
                                        self.resolve_event_from_cell(from_cell)
                                    })
                                    .unwrap_or_else(|| {
                                        self.alloc_event(
                                            "hold_trigger",
                                            EventSource::Synthetic,
                                            hold_span,
                                        )
                                    });
                                if let Expression::When { arms } = &inner_to.node {
                                    // WHEN arm inside LATEST: wrap outer THEN body in PatternMatch.
                                    // Matching WHEN arms → evaluate outer body; SKIP arms → SKIP.
                                    let source_cell =
                                        self.lower_expr_to_cell(inner_from, "hold_when_source");
                                    let ir_arms: Vec<(IrPattern, IrExpr)> = arms
                                        .iter()
                                        .map(|arm| {
                                            let pattern = self.lower_pattern(&arm.pattern);
                                            // Check if this arm's body is SKIP.
                                            let is_skip =
                                                matches!(&arm.body.node, Expression::Skip);
                                            if is_skip {
                                                (pattern, IrExpr::Constant(IrValue::Skip))
                                            } else {
                                                // Matching arm → evaluate outer THEN body.
                                                (pattern, outer_body_expr.clone())
                                            }
                                        })
                                        .collect();
                                    let body_expr = IrExpr::PatternMatch {
                                        source: source_cell,
                                        arms: ir_arms,
                                    };
                                    trigger_bodies.push((trigger, body_expr));
                                } else {
                                    // THEN arm or other pipe inside LATEST:
                                    // trigger fires → evaluate outer THEN body directly.
                                    trigger_bodies.push((trigger, outer_body_expr.clone()));
                                }
                            }
                        }
                        if trigger_bodies.is_empty() {
                            let trigger =
                                self.alloc_event("hold_trigger", EventSource::Synthetic, hold_span);
                            trigger_bodies.push((trigger, outer_body_expr));
                        }
                        return trigger_bodies;
                    }
                    // Simple case: single event source |> THEN { body }
                    let trigger = self
                        .resolve_event_from_expr(&from.node)
                        .or_else(|| {
                            let from_cell = self.lower_expr_to_cell(from, "hold_trigger_source");
                            self.resolve_event_from_cell(from_cell)
                        })
                        .unwrap_or_else(|| {
                            self.alloc_event("hold_trigger", EventSource::Synthetic, hold_span)
                        });
                    let body_expr = self.lower_expr(&then_body.node, then_body.span);
                    return vec![(trigger, body_expr)];
                }
                // Other pipe in HOLD body — lower as expression.
                let expr = self.lower_pipe_to_expr(from, to);
                let trigger = self.alloc_event("hold_trigger", EventSource::Synthetic, hold_span);
                vec![(trigger, expr)]
            }
            // `LATEST { ... }` inside HOLD body — extract triggers from each arm,
            // each with its own body expression.
            Expression::Latest { inputs } => {
                let mut trigger_bodies = Vec::new();
                for input in inputs {
                    if let Expression::Pipe { from, to } = &input.node {
                        if let Expression::Then { body: then_body } = &to.node {
                            let trigger = self
                                .resolve_event_from_expr(&from.node)
                                .or_else(|| {
                                    let from_cell =
                                        self.lower_expr_to_cell(from, "hold_trigger_source");
                                    self.resolve_event_from_cell(from_cell)
                                })
                                .unwrap_or_else(|| {
                                    self.alloc_event(
                                        "hold_trigger",
                                        EventSource::Synthetic,
                                        hold_span,
                                    )
                                });
                            let body_expr = self.lower_expr(&then_body.node, then_body.span);
                            trigger_bodies.push((trigger, body_expr));
                        } else if let Expression::When { arms } = &to.node {
                            // `event_source |> WHEN { pattern => body }` inside HOLD/LATEST.
                            // Extract the trigger from the event source chain (same logic
                            // as THEN arms). Instead of creating a separate WHEN node,
                            // produce an inline PatternMatch expression so the HOLD handler
                            // evaluates the match directly (avoiding ordering issues with
                            // downstream updates).
                            let trigger = self
                                .resolve_event_from_expr(&from.node)
                                .or_else(|| {
                                    let from_cell =
                                        self.lower_expr_to_cell(from, "hold_trigger_source");
                                    self.resolve_event_from_cell(from_cell)
                                })
                                .unwrap_or_else(|| {
                                    self.alloc_event(
                                        "hold_trigger",
                                        EventSource::Synthetic,
                                        hold_span,
                                    )
                                });
                            // Lower the source to a cell (key_data_cell).
                            let source_cell = self.lower_expr_to_cell(from, "hold_when_source");
                            // Lower pattern arms inline.
                            let ir_arms: Vec<(IrPattern, IrExpr)> = arms
                                .iter()
                                .map(|arm| {
                                    let pattern = self.lower_pattern(&arm.pattern);
                                    let saved = if let IrPattern::Binding(ref name) = pattern {
                                        let prev = self.name_to_cell.get(name).copied();
                                        self.name_to_cell.insert(name.clone(), source_cell);
                                        Some((name.clone(), prev))
                                    } else {
                                        None
                                    };
                                    let body = self.lower_expr(&arm.body.node, arm.body.span);
                                    if let Some((name, prev)) = saved {
                                        if let Some(prev_cell) = prev {
                                            self.name_to_cell.insert(name, prev_cell);
                                        } else {
                                            self.name_to_cell.remove(&name);
                                        }
                                    }
                                    (pattern, body)
                                })
                                .collect();
                            let body_expr = IrExpr::PatternMatch {
                                source: source_cell,
                                arms: ir_arms,
                            };
                            trigger_bodies.push((trigger, body_expr));
                        } else {
                            let expr = self.lower_pipe_to_expr(from, to);
                            if let IrExpr::CellRead(cell) = &expr {
                                if let Some(IrNode::Latest { arms, .. }) =
                                    self.nodes.iter().rev().find(|node| {
                                        matches!(node, IrNode::Latest { target, .. } if *target == *cell)
                                    })
                                {
                                    for arm in arms {
                                        if let Some(trigger) = arm.trigger {
                                            trigger_bodies.push((trigger, arm.body.clone()));
                                        }
                                    }
                                } else if let Some(trigger) = self.resolve_event_from_cell(*cell) {
                                    trigger_bodies.push((trigger, expr));
                                }
                            }
                        }
                    } else {
                        let expr = self.lower_expr(&input.node, input.span);
                        if let Some(arms) = self.extract_latest_arms_from_expr(&expr, (0, u32::MAX))
                        {
                            for arm in arms {
                                if let Some(trigger) = arm.trigger {
                                    trigger_bodies.push((trigger, arm.body.clone()));
                                }
                            }
                        }
                    }
                }
                if trigger_bodies.is_empty() {
                    let trigger =
                        self.alloc_event("hold_trigger", EventSource::Synthetic, hold_span);
                    trigger_bodies.push((trigger, IrExpr::Constant(IrValue::Void)));
                }
                trigger_bodies
            }
            // Plain expression — try to resolve an event from the expression itself.
            // This handles bare alias paths like `elements.celsius_input.event.change.text`
            // where the expression references an event payload directly (the HOLD should
            // trigger on that event and read the payload as its new value).
            _ => {
                let expr = self.lower_expr(body, body_span);
                if let Some(arms) = self.extract_latest_arms_from_expr(&expr, (0, u32::MAX)) {
                    let extracted: Vec<(EventId, IrExpr)> = arms
                        .into_iter()
                        .filter_map(|arm| arm.trigger.map(|trigger| (trigger, arm.body)))
                        .collect();
                    if !extracted.is_empty() {
                        return extracted;
                    }
                }
                let trigger = self.resolve_event_from_expr(body).unwrap_or_else(|| {
                    self.alloc_event("hold_trigger", EventSource::Synthetic, hold_span)
                });
                vec![(trigger, expr)]
            }
        }
    }

    /// Try to lower a HOLD with ObjectConstruct init and `N |> Stream/pulses() |> THEN { body }`
    /// pattern as a HoldLoop node. Returns Some(IrNode::HoldLoop) on success.
    fn try_lower_hold_loop(
        &mut self,
        state_name: &str,
        target: CellId,
        init_expr: &IrExpr,
        body: &Expression,
        body_span: Span,
        init_span: Span,
    ) -> Option<IrNode> {
        // Init must be an ObjectConstruct.
        let init_fields = match init_expr {
            IrExpr::ObjectConstruct(fields) => fields.clone(),
            _ => return None,
        };

        // Body must be `N |> Stream/pulses() |> THEN { body_expr }`.
        // Parser produces left-associative pipes: Pipe(Pipe(N, Stream/pulses()), THEN { body })
        let (count_from, then_body) = match body {
            Expression::Pipe { from, to } => {
                if let Expression::Then { body: then_body } = &to.node {
                    (&**from, &**then_body)
                } else {
                    return None;
                }
            }
            _ => return None,
        };

        // count_from should be `N |> Stream/pulses()`, i.e., Pipe(N, Stream/pulses())
        let count_expr_ast = match &count_from.node {
            Expression::Pipe { from, to } => {
                if let Expression::FunctionCall { path, .. } = &to.node {
                    let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                    if path_strs.as_slice() == ["Stream", "pulses"] {
                        &**from
                    } else {
                        return None;
                    }
                } else {
                    return None;
                }
            }
            _ => return None,
        };

        // Allocate field cells and register them so body references resolve correctly.
        let field_cells: Vec<(String, CellId)> = init_fields
            .iter()
            .map(|(name, _)| {
                let cell_name = format!("{}.{}", state_name, name);
                let cell = self.alloc_cell(&cell_name, init_span);
                (name.clone(), cell)
            })
            .collect();

        // Register field cells in name_to_cell.
        let mut saved_bindings: Vec<(String, Option<CellId>)> = Vec::new();
        for (name, cell) in &field_cells {
            let key = format!("{}.{}", state_name, name);
            saved_bindings.push((key.clone(), self.name_to_cell.get(&key).copied()));
            self.name_to_cell.insert(key, *cell);
        }
        // Also bind state_name → target so `state` alone resolves.
        let saved_state = self.name_to_cell.get(state_name).copied();
        self.name_to_cell.insert(state_name.to_string(), target);

        // Register field cells in cell_field_cells for downstream resolution.
        let field_map: HashMap<String, CellId> = field_cells.iter().cloned().collect();
        self.cell_field_cells.insert(target, field_map);

        // Lower the count expression and THEN body.
        let count_expr = self.lower_expr(&count_expr_ast.node, count_expr_ast.span);
        let body_expr = self.lower_expr(&then_body.node, then_body.span);

        // Restore bindings.
        if let Some(prev) = saved_state {
            self.name_to_cell.insert(state_name.to_string(), prev);
        } else {
            self.name_to_cell.remove(state_name);
        }
        for (key, prev) in saved_bindings {
            if let Some(prev_cell) = prev {
                self.name_to_cell.insert(key, prev_cell);
            } else {
                self.name_to_cell.remove(&key);
            }
        }

        // Decompose body into per-field expressions.
        let body_fields = match body_expr {
            IrExpr::ObjectConstruct(fields) => fields,
            _ => {
                // Body is not an ObjectConstruct — can't decompose.
                return None;
            }
        };

        // Verify field names match.
        if body_fields.len() != init_fields.len() {
            return None;
        }

        Some(IrNode::HoldLoop {
            cell: target,
            field_cells,
            init_values: init_fields,
            count_expr,
            body_fields,
        })
    }

    /// Try to resolve an expression to an EventId.
    /// Works for:
    /// - `foo.event.bar` where foo is an element with a LINK for bar
    /// - `foo.bar.event.baz` where foo.bar is a compound element name (object-flattened)
    /// - Simple alias that refers to a cell with an associated event (e.g., Timer output)
    fn resolve_event_from_expr(&self, expr: &Expression) -> Option<EventId> {
        match expr {
            Expression::Alias(Alias::WithoutPassed { parts, .. }) => {
                // Look for the "event" keyword in the path.
                // Everything before "event" is the element name, the part after is the event name.
                if let Some(event_idx) = parts.iter().position(|p| p.as_str() == "event") {
                    if event_idx > 0 && event_idx + 1 < parts.len() {
                        let element_name = parts[..event_idx]
                            .iter()
                            .map(|p| p.as_str())
                            .collect::<Vec<_>>()
                            .join(".");
                        let event_name = parts[event_idx + 1].as_str();
                        if let Some(events) = self.element_events.get(&element_name) {
                            return events.get(event_name).copied();
                        }
                        if let Some(resolved_name) =
                            self.resolve_link_alias_through_cells(&parts[..event_idx])
                        {
                            if let Some(events) = self.element_events.get(&resolved_name) {
                                return events.get(event_name).copied();
                            }
                        }
                        if let Some(resolved_cell) =
                            self.resolve_alias_path_to_cell(&parts[..event_idx])
                        {
                            if let Some(event_id) =
                                self.find_element_event_for_cell(resolved_cell, event_name)
                            {
                                return Some(event_id);
                            }
                        }
                        // Try resolving through alias → global name.
                        // E.g., "elements.clear_button" → cell with global name
                        // "store.elements.clear_button", then look up element_events.
                        if let Some(&cell) = self.name_to_cell.get(&element_name) {
                            let global_name = &self.cells[cell.0 as usize].name;
                            if let Some(events) = self.element_events.get(global_name) {
                                return events.get(event_name).copied();
                            }
                        }
                        // Try resolving just the first part to get a prefix cell,
                        // then build global name with remaining parts.
                        if event_idx > 1 {
                            let first = parts[0].as_str();
                            if let Some(&cell) = self.name_to_cell.get(first) {
                                let cell_global = &self.cells[cell.0 as usize].name;
                                let rest: String = parts[1..event_idx]
                                    .iter()
                                    .map(|p| p.as_str())
                                    .collect::<Vec<_>>()
                                    .join(".");
                                let global_element = format!("{}.{}", cell_global, rest);
                                if let Some(events) = self.element_events.get(&global_element) {
                                    return events.get(event_name).copied();
                                }
                            }
                        }
                    }
                }
                // Look for simple alias → cell with event
                if parts.len() == 1 {
                    if let Some(&cell) = self.name_to_cell.get(parts[0].as_str()) {
                        return self.cell_events.get(&cell).copied();
                    }
                }
                // Try compound name → cell with event
                if parts.len() > 1 {
                    let compound = parts
                        .iter()
                        .map(|p| p.as_str())
                        .collect::<Vec<_>>()
                        .join(".");
                    if let Some(&cell) = self.name_to_cell.get(&compound) {
                        return self.cell_events.get(&cell).copied();
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Check if an expression is a per-item event reference (alias starting with item_name
    /// and containing ".event." in the path).
    /// E.g., `item.todo_elements.remove_todo_button.event.press` with item_name="item" → true.
    fn is_per_item_event_expr(expr: &Expression, item_name: &str) -> bool {
        if let Expression::Alias(Alias::WithoutPassed { parts, .. }) = expr {
            if parts.len() >= 3
                && parts[0].as_str() == item_name
                && parts.iter().any(|p| p.as_str() == "event")
            {
                return true;
            }
        }
        false
    }

    /// Try to find a reactive cell dependency in an expression (for List/append watch_cell).
    /// Walks the expression tree looking for alias references that map to known cells.
    fn find_reactive_cell_in_expr(&self, expr: &Expression) -> Option<CellId> {
        match expr {
            Expression::Alias(Alias::WithoutPassed { parts, .. }) => {
                // Try single-part alias (e.g., `title_to_add`)
                if parts.len() == 1 {
                    if let Some(&cell) = self.name_to_cell.get(parts[0].as_str()) {
                        return Some(cell);
                    }
                }
                // Try compound name
                let compound = parts
                    .iter()
                    .map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                self.name_to_cell.get(&compound).copied()
            }
            Expression::Pipe { from, .. } => {
                // For `a |> b`, the reactive source is in `a`
                self.find_reactive_cell_in_expr(&from.node)
            }
            _ => None,
        }
    }

    /// Try to resolve a CellId to an EventId (for pipe chains where the source is already a cell).
    fn resolve_event_from_cell(&self, cell: CellId) -> Option<EventId> {
        let mut seen = std::collections::HashSet::new();
        self.resolve_event_from_cell_inner(cell, &mut seen)
    }

    fn resolve_event_from_cell_inner(
        &self,
        cell: CellId,
        seen: &mut std::collections::HashSet<CellId>,
    ) -> Option<EventId> {
        let mut current = cell;

        while seen.insert(current) {
            if let Some(event) = self.cell_events.get(&current).copied() {
                return Some(event);
            }
            if let Some((event_idx, _)) = self
                .events
                .iter()
                .enumerate()
                .find(|(_, event)| event.payload_cells.contains(&current))
            {
                return Some(EventId(event_idx as u32));
            }

            let mut advanced = false;
            for node in self.nodes.iter().rev() {
                match node {
                    IrNode::Derived {
                        cell: c,
                        expr: IrExpr::CellRead(src),
                    } if *c == current => {
                        current = *src;
                        advanced = true;
                        break;
                    }
                    IrNode::Derived { cell: c, expr } if *c == current => {
                        if let Some(event) = self.resolve_event_from_ir_expr_inner(expr, seen) {
                            return Some(event);
                        }
                        if let Some(field_cell) = self.resolve_field_access_expr_to_cell(expr) {
                            current = field_cell;
                            advanced = true;
                            break;
                        }
                    }
                    IrNode::PipeThrough { cell: c, source } if *c == current => {
                        current = *source;
                        advanced = true;
                        break;
                    }
                    _ => {}
                }
            }

            if !advanced {
                break;
            }
        }

        None
    }

    fn resolve_event_from_ir_expr(&self, expr: &IrExpr) -> Option<EventId> {
        let mut seen = std::collections::HashSet::new();
        self.resolve_event_from_ir_expr_inner(expr, &mut seen)
    }

    fn resolve_event_from_ir_expr_inner(
        &self,
        expr: &IrExpr,
        seen: &mut std::collections::HashSet<CellId>,
    ) -> Option<EventId> {
        match expr {
            IrExpr::CellRead(cell) => self.resolve_event_from_cell_inner(*cell, seen),
            IrExpr::FieldAccess { object, field } => {
                if let Some(field_cell) = self.resolve_field_access_expr_to_cell(expr) {
                    if let Some(event) = self.resolve_event_from_cell_inner(field_cell, seen) {
                        return Some(event);
                    }
                }

                if let IrExpr::FieldAccess {
                    object: element_expr,
                    field: event_field,
                } = &**object
                {
                    if event_field == "event" {
                        if let Some(element_cell) =
                            self.resolve_field_access_expr_to_cell(element_expr)
                        {
                            if let Some(event) =
                                self.find_element_event_for_cell(element_cell, field)
                            {
                                return Some(event);
                            }
                            let element_name = &self.cells[element_cell.0 as usize].name;
                            if let Some(events) = self.element_events.get(element_name) {
                                if let Some(&event) = events.get(field) {
                                    return Some(event);
                                }
                            }
                        }
                    }
                }

                if matches!(field.as_str(), "key" | "value" | "text") {
                    return self.resolve_event_from_ir_expr_inner(object, seen);
                }

                None
            }
            _ => None,
        }
    }

    fn resolve_field_access_expr_to_cell(&self, expr: &IrExpr) -> Option<CellId> {
        match expr {
            IrExpr::CellRead(cell) => Some(*cell),
            IrExpr::FieldAccess { object, field } => {
                let object_cell = self.resolve_field_access_expr_to_cell(object)?;
                let object_fields = self.resolve_cell_field_cells(object_cell)?;
                object_fields.get(field).copied()
            }
            _ => None,
        }
    }

    fn find_immediate_source_cell(&self, cell: CellId) -> Option<CellId> {
        self.nodes.iter().rev().find_map(|node| match node {
            IrNode::Derived {
                cell: c,
                expr: IrExpr::CellRead(src),
            } if *c == cell => Some(*src),
            IrNode::PipeThrough { cell: c, source } if *c == cell => Some(*source),
            _ => None,
        })
    }

    fn find_metadata_source_cell(&self, cell: CellId) -> Option<CellId> {
        let mut current = cell;
        let mut seen = HashSet::new();
        for _ in 0..20 {
            if !seen.insert(current) {
                return None;
            }
            let next = self.nodes.iter().rev().find_map(|node| match node {
                IrNode::Derived {
                    cell: c,
                    expr: IrExpr::CellRead(src),
                } if *c == current => Some(*src),
                IrNode::Derived {
                    cell: c,
                    expr: IrExpr::FieldAccess { object, field },
                } if *c == current => {
                    self.resolve_field_access_expr_to_cell(object)
                        .and_then(|object_cell| {
                            self.resolve_cell_field_cells(object_cell)
                                .and_then(|fields| fields.get(field).copied())
                                .and_then(|field_cell| {
                                    if field_cell == current {
                                        self.find_immediate_source_cell(field_cell)
                                    } else {
                                        Some(field_cell)
                                    }
                                })
                                .or_else(|| {
                                    self.resolve_cell_to_inline_object(object_cell).and_then(
                                        |fields| {
                                            fields.iter().find(|(name, _)| name == field).and_then(
                                                |(_, expr)| {
                                                    let reduced =
                                                        self.reduce_representative_expr(expr);
                                                    match reduced {
                                                        IrExpr::CellRead(cell) => {
                                                            if cell == current {
                                                                self.find_immediate_source_cell(
                                                                    cell,
                                                                )
                                                            } else {
                                                                Some(cell)
                                                            }
                                                        }
                                                        _ => self
                                                            .resolve_field_access_expr_to_cell(
                                                                &reduced,
                                                            ),
                                                    }
                                                },
                                            )
                                        },
                                    )
                                })
                        })
                }
                IrNode::Derived { cell: c, expr } if *c == current => {
                    self.resolve_field_access_expr_to_cell(expr)
                }
                IrNode::PipeThrough { cell: c, source } if *c == current => Some(*source),
                _ => None,
            });
            match next {
                Some(next) if next != current => current = next,
                _ => return None,
            }
        }
        None
    }

    /// Resolve a LINK target alias to a cell name string.
    /// Used for `|> LINK { target }` to find the LINK placeholder's name
    /// so element events can be associated with it.
    fn resolve_link_target_name(&mut self, alias: &Alias, span: Span) -> Option<String> {
        match alias {
            Alias::WithPassed { extra_parts } => {
                // PASSED path: resolve through PASSED context to find the cell name.
                let passed = self.current_passed.clone()?;
                let mut current_expr = &passed.node;
                let mut parts_iter = extra_parts.iter();
                let mut walked_parts: Vec<String> = Vec::new();

                while let Some(part) = parts_iter.next() {
                    let field = part.as_str();
                    walked_parts.push(field.to_string());
                    match current_expr {
                        Expression::Object(obj) => {
                            let found =
                                obj.variables.iter().find(|v| v.node.name.as_str() == field);
                            if let Some(var) = found {
                                current_expr = &var.node.value.node;
                            } else {
                                return None;
                            }
                        }
                        Expression::Alias(inner_alias) => {
                            // Resolve nested aliases (including nested PASSED paths)
                            // to a concrete base name, then append remaining parts.
                            if let Some(mut name) = self.resolve_link_target_name(inner_alias, span)
                            {
                                name.push('.');
                                name.push_str(field);
                                for remaining in parts_iter {
                                    name.push('.');
                                    name.push_str(remaining.as_str());
                                }
                                return Some(name);
                            }
                            return None;
                        }
                        _ => return None,
                    }
                }

                // If we consumed all parts and landed on a Link placeholder, find its cell name.
                if matches!(current_expr, Expression::Link) {
                    self.resolve_link_target_name_from_parts(&walked_parts)
                } else if let Expression::Alias(inner) = current_expr {
                    if let Alias::WithoutPassed { parts, .. } = inner {
                        if let Some(&cell) = self.name_to_cell.get(parts[0].as_str()) {
                            return Some(self.cells[cell.0 as usize].name.clone());
                        }
                    }
                    None
                } else {
                    None
                }
            }
            Alias::WithoutPassed { parts, .. } => {
                // Direct name resolution.
                let compound = parts
                    .iter()
                    .map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                if self.element_events.contains_key(&compound)
                    || self.name_to_cell.contains_key(&compound)
                {
                    Some(compound)
                } else if parts.len() == 1 {
                    if self.element_events.contains_key(parts[0].as_str()) {
                        Some(parts[0].as_str().to_string())
                    } else {
                        None
                    }
                } else {
                    // Multi-part alias: resolve through cell hierarchy.
                    // e.g., "item.item_elements.toggle_button" → find the cell,
                    // then use the cell's name to look up element_events.
                    self.resolve_link_alias_through_cells(parts)
                }
            }
        }
    }

    fn resolve_link_target_name_from_parts(&self, parts: &[String]) -> Option<String> {
        let first = parts.first()?;
        let mut current_cell = self.name_to_cell.get(first).copied()?;

        for field in &parts[1..] {
            if let Some(fields) = self.resolve_cell_field_cells(current_cell) {
                if let Some(&field_cell) = fields.get(field) {
                    current_cell = field_cell;
                    continue;
                }
            }

            let cell_name = &self.cells[current_cell.0 as usize].name;
            let global_path = format!("{}.{}", cell_name, field);
            if let Some(&cell) = self.name_to_cell.get(&global_path) {
                current_cell = cell;
            } else {
                return None;
            }
        }

        Some(self.cells[current_cell.0 as usize].name.clone())
    }

    /// Resolve a multi-part alias (e.g., "item.item_elements.toggle_button")
    /// through the cell hierarchy to find the actual cell name for element_events lookup.
    fn resolve_link_alias_through_cells(
        &self,
        parts: &[crate::parser::StrSlice],
    ) -> Option<String> {
        let current_cell = self.resolve_alias_path_to_cell(parts)?;

        // Return the resolved cell's name for element_events lookup.
        let cell_name = self.cells[current_cell.0 as usize].name.clone();
        if self.element_events.contains_key(&cell_name) {
            Some(cell_name)
        } else {
            // Cell found but no events pre-allocated for it.
            // Still return the name so current_var_name can be set.
            Some(cell_name)
        }
    }

    fn resolve_alias_path_to_cell(&self, parts: &[crate::parser::StrSlice]) -> Option<CellId> {
        let compound_cell = if parts.len() > 1 {
            let compound = parts
                .iter()
                .map(|part| part.as_str())
                .collect::<Vec<_>>()
                .join(".");
            self.name_to_cell.get(&compound).copied()
        } else {
            None
        };

        // Start with the first part and find its cell.
        let first = parts[0].as_str();
        let Some(mut current_cell) = self.name_to_cell.get(first).copied() else {
            return compound_cell;
        };

        if parts.len() > 1 {
            let mut walked_all_fields = true;
            for part in &parts[1..] {
                let field = part.as_str();
                let cell_name = &self.cells[current_cell.0 as usize].name;
                let global_path = format!("{}.{}", cell_name, field);
                let global_cell = self.name_to_cell.get(&global_path).copied();
                if let Some(fields) = self.resolve_cell_field_cells(current_cell)
                    && let Some(&field_cell) = fields.get(field)
                {
                    let prefer_global = global_cell.filter(|candidate| {
                        self.cell_has_concrete_shape(*candidate)
                            || self.find_list_constructor(*candidate).is_some()
                            || self.find_list_item_field_exprs(*candidate).is_some()
                    });
                    current_cell = prefer_global.unwrap_or(field_cell);
                    continue;
                }
                if let Some(cell) = global_cell {
                    current_cell = cell;
                } else {
                    walked_all_fields = false;
                    break;
                }
            }
            if walked_all_fields {
                return Some(current_cell);
            }
            if let Some(cell) = compound_cell {
                return Some(cell);
            }
        } else if let Some(cell) = compound_cell {
            return Some(cell);
        }

        // Follow remaining parts through cell_field_cells.
        for part in &parts[1..] {
            let field = part.as_str();
            let cell_name = &self.cells[current_cell.0 as usize].name;
            let global_path = format!("{}.{}", cell_name, field);
            let global_cell = self.name_to_cell.get(&global_path).copied();
            // Check cell_field_cells for a direct field lookup.
            if let Some(fields) = self.resolve_cell_field_cells(current_cell) {
                if let Some(&field_cell) = fields.get(field) {
                    let prefer_global = global_cell.filter(|candidate| {
                        self.cell_has_concrete_shape(*candidate)
                            || self.find_list_constructor(*candidate).is_some()
                            || self.find_list_item_field_exprs(*candidate).is_some()
                    });
                    current_cell = prefer_global.unwrap_or(field_cell);
                    continue;
                }
            }
            // Also try global name + field in name_to_cell.
            if let Some(cell) = global_cell {
                current_cell = cell;
            } else {
                return None;
            }
        }
        Some(current_cell)
    }

    fn find_element_event_for_cell(&self, cell: CellId, event_name: &str) -> Option<EventId> {
        self.nodes.iter().rev().find_map(|node| match node {
            IrNode::Element {
                cell: node_cell,
                links,
                ..
            } if *node_cell == cell => links
                .iter()
                .find_map(|(name, event_id)| (name == event_name).then_some(*event_id)),
            _ => None,
        })
    }

    fn debug_source_cell_event_resolution(&self, cell: CellId) -> String {
        let cell_name = self
            .cells
            .get(cell.0 as usize)
            .map(|cell| cell.name.as_str())
            .unwrap_or("<unknown>");
        let field_names = self
            .resolve_cell_field_cells(cell)
            .map(|fields| fields.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        let named_events = self
            .element_events
            .get(cell_name)
            .map(|events| events.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        let direct_link_events = self
            .nodes
            .iter()
            .rev()
            .find_map(|node| match node {
                IrNode::Element {
                    cell: node_cell,
                    links,
                    ..
                } if *node_cell == cell => Some(
                    links
                        .iter()
                        .map(|(name, _)| name.clone())
                        .collect::<Vec<_>>(),
                ),
                _ => None,
            })
            .unwrap_or_default();
        format!(
            "source_cell={}({}) cell_event={} field_names={:?} named_events={:?} direct_link_events={:?}",
            cell.0,
            cell_name,
            self.cell_events.contains_key(&cell),
            field_names,
            named_events,
            direct_link_events
        )
    }

    fn debug_event_resolution(&self, expr: &Expression) -> String {
        match expr {
            Expression::Alias(Alias::WithoutPassed { parts, .. }) => {
                if let Some(event_idx) = parts.iter().position(|p| p.as_str() == "event") {
                    if event_idx > 0 && event_idx + 1 < parts.len() {
                        let prefix = &parts[..event_idx];
                        let event_name = parts[event_idx + 1].as_str();
                        let path = prefix
                            .iter()
                            .map(|p| p.as_str())
                            .collect::<Vec<_>>()
                            .join(".");
                        if let Some(cell) = self.resolve_alias_path_to_cell(prefix) {
                            let cell_name = &self.cells[cell.0 as usize].name;
                            let named_events = self
                                .element_events
                                .get(cell_name)
                                .map(|events| events.keys().cloned().collect::<Vec<_>>())
                                .unwrap_or_default();
                            let linked =
                                self.find_element_event_for_cell(cell, event_name).is_some();
                            return format!(
                                "alias_path={} resolved_cell={}({}) named_events={:?} direct_link_event={}",
                                path, cell.0, cell_name, named_events, linked
                            );
                        }
                        let first = prefix.first().map(|part| part.as_str()).unwrap_or("<none>");
                        let first_cell = self.name_to_cell.get(first).copied();
                        let first_debug = first_cell.map(|cell| {
                            let cell_name = &self.cells[cell.0 as usize].name;
                            let field_names = self
                                .resolve_cell_field_cells(cell)
                                .map(|fields| fields.keys().cloned().collect::<Vec<_>>())
                                .unwrap_or_default();
                            format!("{}({}) fields={:?}", cell.0, cell_name, field_names)
                        });
                        let mut partials = Vec::new();
                        for len in 1..=prefix.len() {
                            let partial = prefix[..len]
                                .iter()
                                .map(|part| part.as_str())
                                .collect::<Vec<_>>()
                                .join(".");
                            let resolved = self
                                .resolve_alias_path_to_cell(&prefix[..len])
                                .map(|cell| {
                                    format!("{}({})", cell.0, self.cells[cell.0 as usize].name)
                                })
                                .unwrap_or_else(|| "<none>".to_string());
                            partials.push(format!("{partial}->{resolved}"));
                        }
                        return format!(
                            "alias_path={} resolved_cell=<none> first_binding={:?} partials={:?}",
                            path, first_debug, partials
                        );
                    }
                }
                let raw = parts
                    .iter()
                    .map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                format!("alias={raw}")
            }
            _ => format!("expr={expr:?}"),
        }
    }

    /// Convert a Duration expression to milliseconds.
    /// Handles `Duration[seconds: N]`, `Duration[ms: N]`, and plain numbers.
    /// Returns a constant f64 expression (ms value).
    fn lower_duration_to_ms(&mut self, expr: &Expression, span: Span) -> IrExpr {
        match expr {
            Expression::TaggedObject { tag, object } if tag.as_str() == "Duration" => {
                for v in &object.variables {
                    let name = v.node.name.as_str();
                    match name {
                        "seconds" => {
                            // Extract numeric value and multiply by 1000 at compile time.
                            if let Expression::Literal(Literal::Number(n)) = &v.node.value.node {
                                return IrExpr::Constant(IrValue::Number(n * 1000.0));
                            }
                            // Non-constant: emit as BinOp.
                            let val = self.lower_expr(&v.node.value.node, v.node.value.span);
                            return IrExpr::BinOp {
                                op: BinOp::Mul,
                                lhs: Box::new(val),
                                rhs: Box::new(IrExpr::Constant(IrValue::Number(1000.0))),
                            };
                        }
                        "ms" | "milliseconds" => {
                            return self.lower_expr(&v.node.value.node, v.node.value.span);
                        }
                        _ => {}
                    }
                }
                // Default: 1 second.
                IrExpr::Constant(IrValue::Number(1000.0))
            }
            // Plain number → treat as milliseconds.
            _ => self.lower_expr(expr, span),
        }
    }

    /// Lower an expression to an IrExpr (pure value, no side-effect nodes).
    fn lower_expr(&mut self, expr: &Expression, span: Span) -> IrExpr {
        match expr {
            Expression::Literal(lit) => IrExpr::Constant(self.lower_literal(lit)),

            Expression::Alias(alias) => self.lower_alias(alias, span),

            Expression::ArithmeticOperator(op) => self.lower_arithmetic(op),

            Expression::Comparator(cmp) => self.lower_comparator(cmp),

            Expression::TextLiteral { parts, .. } => {
                let segments: Vec<TextSegment> = parts
                    .iter()
                    .map(|p| match p {
                        TextPart::Text(s) => TextSegment::Literal(s.as_str().to_string()),
                        TextPart::Interpolation {
                            var,
                            referenced_span,
                        } => {
                            let name = var.as_str();
                            // Handle dotted paths like "a.b.c" and PASSED paths.
                            let parts: Vec<&str> = name.split('.').collect();
                            if parts[0] == "PASSED" {
                                // Resolve through PASSED context.
                                let extra: Vec<_> = parts[1..].to_vec();
                                let expr = self.resolve_passed_text_interp(&extra, span);
                                TextSegment::Expr(expr)
                            } else if parts.len() > 1 {
                                // Try full dotted path first (e.g., "store.counter" → cell 3).
                                if let Some(&cell) = self.name_to_cell.get(name) {
                                    TextSegment::Expr(IrExpr::CellRead(cell))
                                } else {
                                    // Try progressively longer prefixes.
                                    let mut resolved = false;
                                    let mut result_expr = IrExpr::Constant(IrValue::Void);
                                    // Try resolving first part, then build FieldAccess for rest.
                                    if let Some(&cell) = self.name_to_cell.get(parts[0]) {
                                        // Try resolving with cell_field_cells for remaining parts.
                                        let mut current_cell = cell;
                                        let mut all_resolved = true;
                                        for part in &parts[1..] {
                                            if let Some(fields) =
                                                self.cell_field_cells.get(&current_cell)
                                            {
                                                if let Some(&field_cell) = fields.get(*part) {
                                                    current_cell = field_cell;
                                                    continue;
                                                }
                                            }
                                            all_resolved = false;
                                            break;
                                        }
                                        if all_resolved {
                                            result_expr = IrExpr::CellRead(current_cell);
                                            resolved = true;
                                        } else {
                                            // Fallback to FieldAccess chain.
                                            let mut expr = IrExpr::CellRead(cell);
                                            for part in &parts[1..] {
                                                expr = IrExpr::FieldAccess {
                                                    object: Box::new(expr),
                                                    field: part.to_string(),
                                                };
                                            }
                                            result_expr = expr;
                                            resolved = true;
                                        }
                                    }
                                    if resolved {
                                        TextSegment::Expr(result_expr)
                                    } else {
                                        TextSegment::Literal(format!("{{{}}}", name))
                                    }
                                }
                            } else if let Some(&cell) = self.name_to_cell.get(parts[0]) {
                                TextSegment::Expr(IrExpr::CellRead(cell))
                            } else {
                                TextSegment::Literal(format!("{{{}}}", name))
                            }
                        }
                    })
                    .collect();
                IrExpr::TextConcat(segments)
            }

            Expression::Object(obj) => {
                // Check if the object has reactive fields (LINK, HOLD, Pipe, LATEST,
                // nested objects, etc.) that need cell allocation and name registration.
                let has_reactive_fields = obj.variables.iter().any(|v| {
                    v.node.name.is_empty() // Spread entry — needs object-store for field merging
                        || matches!(
                            v.node.value.node,
                            Expression::Link
                                | Expression::Hold { .. }
                                | Expression::Latest { .. }
                                | Expression::Pipe { .. }
                                | Expression::Object(_)
                                | Expression::FunctionCall { .. }
                        )
                });
                if has_reactive_fields || self.force_object_store {
                    // Use the object-as-store pattern: allocate cells for each field,
                    // register names, then lower values. This enables cross-field references.
                    let parent_cell = self.alloc_cell("object", span);
                    self.lower_object_store(parent_cell, obj, span);
                    IrExpr::CellRead(parent_cell)
                } else {
                    let fields: Vec<(String, IrExpr)> = obj
                        .variables
                        .iter()
                        .filter(|v| !v.node.name.is_empty()) // Skip spread entries
                        .map(|v| {
                            let name = v.node.name.as_str().to_string();
                            let val = self.lower_expr(&v.node.value.node, v.node.value.span);
                            let val = self.reduce_representative_expr(&val);
                            (name, val)
                        })
                        .collect();
                    IrExpr::ObjectConstruct(fields)
                }
            }

            Expression::TaggedObject { tag, object } => {
                let fields: Vec<(String, IrExpr)> = object
                    .variables
                    .iter()
                    .filter(|v| !v.node.name.is_empty()) // Skip spread entries
                    .map(|v| {
                        let name = v.node.name.as_str().to_string();
                        let val = self.lower_expr(&v.node.value.node, v.node.value.span);
                        let val = self.reduce_representative_expr(&val);
                        (name, val)
                    })
                    .collect();
                IrExpr::TaggedObject {
                    tag: tag.as_str().to_string(),
                    fields,
                }
            }

            Expression::List { items } => {
                // Detect item constructor function from first item.
                // If items are function calls, record the function name so List/map
                // can re-inline the constructor to create per-item cells.
                if let Some(first_item) = items.first() {
                    if let Expression::FunctionCall { path, .. } = &first_item.node {
                        let fn_name = path.last().map(|s| s.as_str().to_string());
                        if let Some(name) = fn_name {
                            if self.func_defs.contains_key(&name) {
                                self.pending_list_constructor = Some(name);
                            }
                        }
                    }
                }
                // Force object store for list items from constructor calls so they
                // get proper cell_field_cells entries (needed for text resolution).
                let has_constructor = self.pending_list_constructor.is_some();
                let saved_force = self.force_object_store;
                if has_constructor {
                    self.force_object_store = true;
                }
                let exprs: Vec<IrExpr> = items
                    .iter()
                    .map(|item| self.lower_expr(&item.node, item.span))
                    .collect();
                self.force_object_store = saved_force;
                IrExpr::ListConstruct(exprs)
            }

            Expression::FunctionCall { path, arguments } => {
                self.lower_function_call(path, arguments, span)
            }

            Expression::Block { variables, output } => {
                // Lower block variables as local cells, then return the output expression.
                // IMPORTANT: defer short-name registration until after the value is lowered
                // so a block local can still read an outer binding with the same name.
                let mut saved_bindings: Vec<(String, Option<CellId>)> = Vec::new();
                for v in variables {
                    let name = v.node.name.as_str();
                    let cell = self.alloc_cell(name, v.span);
                    let name_string = name.to_string();
                    let saved = self.name_to_cell.get(name).copied();
                    saved_bindings.push((name_string.clone(), saved));
                    let expr = self.lower_expr(&v.node.value.node, v.node.value.span);
                    self.propagate_expr_field_cells(cell, &expr);
                    if let Some(constructor_name) =
                        self.extract_list_constructor_from_value_expr(&v.node.value.node)
                    {
                        self.list_item_constructor.insert(cell, constructor_name);
                    }
                    let metadata_source = match &expr {
                        IrExpr::CellRead(src) => Some(*src),
                        _ => self.resolve_field_access_expr_to_cell(&expr),
                    };
                    if let Some(src) = metadata_source {
                        self.propagate_list_constructor(src, cell);
                    }
                    self.name_to_cell.insert(name_string, cell);
                    self.nodes.push(IrNode::Derived { cell, expr });
                }
                let result = self.lower_expr(&output.node, output.span);
                for (name, saved) in saved_bindings.into_iter().rev() {
                    if let Some(cell) = saved {
                        self.name_to_cell.insert(name, cell);
                    } else {
                        self.name_to_cell.remove(&name);
                    }
                }
                result
            }

            Expression::Pipe { from, to } => self.lower_pipe_to_expr(from, to),

            Expression::Latest { inputs } => {
                // LATEST inside an expression — allocate a cell and lower it.
                let cell = self.alloc_cell("latest_expr", span);
                self.lower_latest(cell, inputs, span);
                IrExpr::CellRead(cell)
            }

            // Hardware types — not supported in WASM engine.
            Expression::Bits { .. } | Expression::Memory { .. } | Expression::Bytes { .. } => {
                self.error(
                    span,
                    "Hardware types (Bits/Memory/Bytes) are not supported in the WASM engine",
                );
                IrExpr::Constant(IrValue::Void)
            }

            Expression::Variable(var) => {
                // Nested variable definition inside an expression.
                let name = var.name.as_str();
                let cell = self.alloc_cell(name, span);
                self.name_to_cell.insert(name.to_string(), cell);
                self.lower_variable(cell, &var.value, span);
                IrExpr::CellRead(cell)
            }

            Expression::Skip => IrExpr::Constant(IrValue::Skip),

            Expression::Link => IrExpr::Constant(IrValue::Void),

            Expression::LinkSetter { .. } => IrExpr::Constant(IrValue::Void),

            Expression::FieldAccess { path } => {
                // Standalone field access without pipe source — not valid at this position.
                self.error(span, "Field access (.field) is only valid in pipe position");
                IrExpr::Constant(IrValue::Void)
            }

            Expression::PostfixFieldAccess { expr, field } => {
                // expr.field — lower the inner expression, then wrap with FieldAccess
                let inner = self.lower_expr(&expr.node, expr.span);
                let field_expr = IrExpr::FieldAccess {
                    object: Box::new(inner),
                    field: field.as_str().to_string(),
                };
                if let Some(cell) = self.resolve_field_access_expr_to_cell(&field_expr) {
                    IrExpr::CellRead(cell)
                } else {
                    field_expr
                }
            }

            Expression::Hold { .. }
            | Expression::Then { .. }
            | Expression::When { .. }
            | Expression::While { .. } => {
                self.error(
                    span,
                    "HOLD/THEN/WHEN/WHILE must appear on the right side of a pipe (|>)",
                );
                IrExpr::Constant(IrValue::Void)
            }

            Expression::Function { .. } => {
                // Function definition in expression position.
                IrExpr::Constant(IrValue::Void)
            }

            Expression::Map { .. } => {
                self.error(
                    span,
                    "Map literals are not yet supported in the WASM engine",
                );
                IrExpr::Constant(IrValue::Void)
            }

            Expression::Flush { value } => {
                // FLUSH — pass through for now.
                self.lower_expr(&value.node, value.span)
            }

            Expression::Spread { value } => self.lower_expr(&value.node, value.span),
        }
    }

    /// Lower a function call expression.
    fn lower_function_call(
        &mut self,
        path: &[crate::parser::StrSlice],
        arguments: &[Spanned<Argument>],
        span: Span,
    ) -> IrExpr {
        let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();

        // Remap Scene/Element/* → Element/* (Scene elements are just Element aliases)
        let remapped: Vec<&str>;
        let effective =
            if path_strs.len() == 3 && path_strs[0] == "Scene" && path_strs[1] == "Element" {
                remapped = vec!["Element", path_strs[2]];
                &remapped
            } else {
                &path_strs
            };

        match effective.as_slice() {
            // --- Document/new(root: expr) ---
            ["Document", "new"] => {
                if let Some(root_arg) = arguments.iter().find(|a| a.node.name.as_str() == "root") {
                    if let Some(ref val) = root_arg.node.value {
                        let root_cell = self.lower_expr_to_cell(val, "doc_root");
                        self.set_render_root(RenderSurface::Document, root_cell, None, None);
                        return IrExpr::CellRead(root_cell);
                    }
                }
                self.error(span, "Document/new requires a 'root' argument");
                IrExpr::Constant(IrValue::Void)
            }

            // --- Scene/new(root: ..., lights: ..., geometry: ...) ---
            // Preserve root, lights, and geometry so the renderer can decide
            // how much scene-aware fallback behavior it can provide.
            ["Scene", "new"] => {
                if let Some(root_arg) = arguments.iter().find(|a| a.node.name.as_str() == "root") {
                    if let Some(ref val) = root_arg.node.value {
                        let root_cell = self.lower_expr_to_cell(val, "scene_root");
                        let lights = arguments
                            .iter()
                            .find(|a| a.node.name.as_str() == "lights")
                            .and_then(|arg| arg.node.value.as_ref())
                            .map(|value| self.lower_expr_to_cell(value, "scene_lights"));
                        let geometry = arguments
                            .iter()
                            .find(|a| a.node.name.as_str() == "geometry")
                            .and_then(|arg| arg.node.value.as_ref())
                            .map(|value| self.lower_expr_to_cell(value, "scene_geometry"));
                        self.set_render_root(RenderSurface::Scene, root_cell, lights, geometry);
                        return IrExpr::CellRead(root_cell);
                    }
                }
                self.error(span, "Scene/new requires a 'root' argument");
                IrExpr::Constant(IrValue::Void)
            }

            // --- Lights/basic(), Light/*() ---
            ["Lights", "basic"] | ["Lights", "directional"] | ["Lights", "ambient"] => {
                IrExpr::ListConstruct(Vec::new())
            }
            ["Light", "directional"] => IrExpr::TaggedObject {
                tag: "DirectionalLight".to_string(),
                fields: vec![
                    (
                        "azimuth".to_string(),
                        self.find_arg_expr(arguments, "azimuth", span),
                    ),
                    (
                        "altitude".to_string(),
                        self.find_arg_expr(arguments, "altitude", span),
                    ),
                    (
                        "spread".to_string(),
                        self.find_arg_expr(arguments, "spread", span),
                    ),
                    (
                        "intensity".to_string(),
                        self.find_arg_expr(arguments, "intensity", span),
                    ),
                    (
                        "color".to_string(),
                        self.find_arg_expr(arguments, "color", span),
                    ),
                ],
            },
            ["Light", "ambient"] => IrExpr::TaggedObject {
                tag: "AmbientLight".to_string(),
                fields: vec![
                    (
                        "intensity".to_string(),
                        self.find_arg_expr(arguments, "intensity", span),
                    ),
                    (
                        "color".to_string(),
                        self.find_arg_expr(arguments, "color", span),
                    ),
                ],
            },
            ["Light", "spot"] => IrExpr::TaggedObject {
                tag: "SpotLight".to_string(),
                fields: vec![
                    (
                        "target".to_string(),
                        self.find_arg_expr(arguments, "target", span),
                    ),
                    (
                        "color".to_string(),
                        self.find_arg_expr(arguments, "color", span),
                    ),
                    (
                        "intensity".to_string(),
                        self.find_arg_expr(arguments, "intensity", span),
                    ),
                    (
                        "radius".to_string(),
                        self.find_arg_expr(arguments, "radius", span),
                    ),
                    (
                        "softness".to_string(),
                        self.find_arg_expr(arguments, "softness", span),
                    ),
                ],
            },

            // --- Element/button(element: [...], label: TEXT { ... }, style: [...]) ---
            ["Element", "button"] => {
                let saved_elem = self.process_element_self_ref(arguments, span);
                let label = self.find_arg_expr(arguments, "label", span);
                let style = self.find_arg_expr_or_default(arguments, "style");

                let cell = self.alloc_cell("button", span);
                let cell_name = self.cells[cell.0 as usize].name.clone();
                let links = self.extract_links_for_element(&cell_name);
                let hovered_cell = saved_elem.hovered_cell;

                self.nodes.push(IrNode::Element {
                    cell,
                    kind: ElementKind::Button { label, style },
                    links,
                    hovered_cell,
                });
                self.restore_element_self_ref(saved_elem);
                IrExpr::CellRead(cell)
            }

            // --- Element/text_input(element: [...], placeholder: TEXT { ... }, style: [...]) ---
            ["Element", "text_input"] => {
                let saved_elem = self.process_element_self_ref(arguments, span);
                let placeholder = arguments
                    .iter()
                    .find(|a| a.node.name.as_str() == "placeholder")
                    .and_then(|a| a.node.value.as_ref())
                    .map(|v| self.lower_expr(&v.node, v.span));
                let style = self.find_arg_expr_or_default(arguments, "style");
                // Lower the text argument for text_input.
                let cell = self.alloc_cell("text_input", span);
                let links =
                    self.extract_links_for_element(&self.cells[cell.0 as usize].name.clone());
                let hovered_cell = saved_elem.hovered_cell;

                let focus = self.has_element_bool_field(arguments, "focus")
                    || self.has_top_level_bool_arg(arguments, "focus");
                let text_cell = arguments
                    .iter()
                    .find(|a| a.node.name.as_str() == "text")
                    .and_then(|a| a.node.value.as_ref())
                    .map(|v| self.lower_expr_to_cell(v, "text_input_text"));
                if let (Some(prop_cell), Some(source_cell)) = (saved_elem.text_cell, text_cell) {
                    self.nodes.push(IrNode::Derived {
                        cell: prop_cell,
                        expr: IrExpr::CellRead(source_cell),
                    });
                }
                self.nodes.push(IrNode::Element {
                    cell,
                    kind: ElementKind::TextInput {
                        placeholder,
                        style,
                        focus,
                        text_cell,
                    },
                    links,
                    hovered_cell,
                });
                self.restore_element_self_ref(saved_elem);
                IrExpr::CellRead(cell)
            }

            // --- Element/checkbox(element: [...], style: [...], checked: ..., icon: ...) ---
            ["Element", "checkbox"] => {
                let saved_elem = self.process_element_self_ref(arguments, span);
                let style = self.find_arg_expr_or_default(arguments, "style");

                // Lower the `checked` argument to a CellId if present.
                let checked = arguments
                    .iter()
                    .find(|a| a.node.name.as_str() == "checked")
                    .and_then(|a| a.node.value.as_ref())
                    .map(|v| {
                        let expr = self.lower_expr(&v.node, v.span);
                        match expr {
                            IrExpr::CellRead(c) => c,
                            other => {
                                let c = self.alloc_cell("checkbox_checked", span);
                                self.nodes.push(IrNode::Derived {
                                    cell: c,
                                    expr: other,
                                });
                                c
                            }
                        }
                    });

                // Lower the `icon` argument to a CellId if present.
                let icon = arguments
                    .iter()
                    .find(|a| a.node.name.as_str() == "icon")
                    .and_then(|a| a.node.value.as_ref())
                    .map(|v| {
                        let expr = self.lower_expr(&v.node, v.span);
                        match expr {
                            IrExpr::CellRead(c) => c,
                            other => {
                                let c = self.alloc_cell("checkbox_icon", span);
                                self.nodes.push(IrNode::Derived {
                                    cell: c,
                                    expr: other,
                                });
                                c
                            }
                        }
                    });

                let cell = self.alloc_cell("checkbox", span);
                let links =
                    self.extract_links_for_element(&self.cells[cell.0 as usize].name.clone());
                let hovered_cell = saved_elem.hovered_cell;

                self.nodes.push(IrNode::Element {
                    cell,
                    kind: ElementKind::Checkbox {
                        checked,
                        style,
                        icon,
                    },
                    links,
                    hovered_cell,
                });
                self.restore_element_self_ref(saved_elem);
                IrExpr::CellRead(cell)
            }

            // --- Element/container(child: ..., style: [...]) ---
            ["Element", "container"] => {
                let saved_elem = self.process_element_self_ref(arguments, span);
                let child_expr = self.find_arg_expr_or_default(arguments, "child");
                let child_cell = self.alloc_cell("container_child", span);
                self.nodes.push(IrNode::Derived {
                    cell: child_cell,
                    expr: child_expr,
                });
                let style = self.find_arg_expr_or_default(arguments, "style");

                let cell = self.alloc_cell("container", span);
                let links =
                    self.extract_links_for_element(&self.cells[cell.0 as usize].name.clone());
                let hovered_cell = saved_elem.hovered_cell;

                self.nodes.push(IrNode::Element {
                    cell,
                    kind: ElementKind::Container {
                        child: child_cell,
                        style,
                    },
                    links,
                    hovered_cell,
                });
                self.restore_element_self_ref(saved_elem);
                IrExpr::CellRead(cell)
            }

            // --- Element/label(label: ..., style: [...]) ---
            ["Element", "label"] => {
                let saved_elem = self.process_element_self_ref(arguments, span);
                let label = self.find_arg_expr(arguments, "label", span);
                let style = self.find_arg_expr_or_default(arguments, "style");

                let cell = self.alloc_cell("label", span);
                let links =
                    self.extract_links_for_element(&self.cells[cell.0 as usize].name.clone());
                let hovered_cell = saved_elem.hovered_cell;

                self.nodes.push(IrNode::Element {
                    cell,
                    kind: ElementKind::Label { label, style },
                    links,
                    hovered_cell,
                });
                self.restore_element_self_ref(saved_elem);
                IrExpr::CellRead(cell)
            }

            // --- Element/stack(layers: LIST { ... }, style: [...]) ---
            ["Element", "stack"] => {
                let saved_elem = self.process_element_self_ref(arguments, span);
                let style = self.find_arg_expr_or_default(arguments, "style");
                let layers_expr = self.find_arg_expr_or_default(arguments, "layers");
                let layers_cell = self.alloc_cell("stack_layers", span);
                self.nodes.push(IrNode::Derived {
                    cell: layers_cell,
                    expr: layers_expr,
                });

                let cell = self.alloc_cell("stack", span);
                let links =
                    self.extract_links_for_element(&self.cells[cell.0 as usize].name.clone());
                let hovered_cell = saved_elem.hovered_cell;

                self.nodes.push(IrNode::Element {
                    cell,
                    kind: ElementKind::Stack {
                        layers: layers_cell,
                        style,
                    },
                    links,
                    hovered_cell,
                });
                self.restore_element_self_ref(saved_elem);
                IrExpr::CellRead(cell)
            }

            // --- Element/link(url/to: ..., label: ..., style: [...]) ---
            ["Element", "link"] => {
                let saved_elem = self.process_element_self_ref(arguments, span);
                // Accept both "url" and "to" for the link target.
                let url = arguments
                    .iter()
                    .find(|a| {
                        let n = a.node.name.as_str();
                        n == "url" || n == "to"
                    })
                    .and_then(|a| a.node.value.as_ref())
                    .map(|v| self.lower_expr(&v.node, v.span))
                    .unwrap_or(IrExpr::Constant(IrValue::Void));
                let label = self.find_arg_expr(arguments, "label", span);
                let style = self.find_arg_expr_or_default(arguments, "style");

                let cell = self.alloc_cell("link", span);
                let links =
                    self.extract_links_for_element(&self.cells[cell.0 as usize].name.clone());
                let hovered_cell = saved_elem.hovered_cell;

                self.nodes.push(IrNode::Element {
                    cell,
                    kind: ElementKind::Link { url, label, style },
                    links,
                    hovered_cell,
                });
                self.restore_element_self_ref(saved_elem);
                IrExpr::CellRead(cell)
            }

            // --- Element/paragraph(content/contents: ..., style: [...]) ---
            ["Element", "paragraph"] => {
                let saved_elem = self.process_element_self_ref(arguments, span);
                // Accept both "content" (singular) and "contents" (plural).
                let content = arguments
                    .iter()
                    .find(|a| {
                        let n = a.node.name.as_str();
                        n == "content" || n == "contents"
                    })
                    .and_then(|a| a.node.value.as_ref())
                    .map(|v| self.lower_expr(&v.node, v.span))
                    .unwrap_or_else(|| {
                        self.error(
                            span,
                            "Element/paragraph requires a 'content' or 'contents' argument",
                        );
                        IrExpr::Constant(IrValue::Void)
                    });
                let style = self.find_arg_expr_or_default(arguments, "style");

                let cell = self.alloc_cell("paragraph", span);
                let links =
                    self.extract_links_for_element(&self.cells[cell.0 as usize].name.clone());
                let hovered_cell = saved_elem.hovered_cell;

                self.nodes.push(IrNode::Element {
                    cell,
                    kind: ElementKind::Paragraph { content, style },
                    links,
                    hovered_cell,
                });
                self.restore_element_self_ref(saved_elem);
                IrExpr::CellRead(cell)
            }

            // --- Element/block(child: ..., style: [...]) ---
            ["Element", "block"] => {
                let saved_elem = self.process_element_self_ref(arguments, span);
                let child_expr = self.find_arg_expr_or_default(arguments, "child");
                let child_cell = self.alloc_cell("block_child", span);
                self.nodes.push(IrNode::Derived {
                    cell: child_cell,
                    expr: child_expr,
                });
                let style = self.find_arg_expr_or_default(arguments, "style");

                let cell = self.alloc_cell("block", span);
                let links =
                    self.extract_links_for_element(&self.cells[cell.0 as usize].name.clone());
                let hovered_cell = saved_elem.hovered_cell;

                self.nodes.push(IrNode::Element {
                    cell,
                    kind: ElementKind::Block {
                        child: child_cell,
                        style,
                    },
                    links,
                    hovered_cell,
                });
                self.restore_element_self_ref(saved_elem);
                IrExpr::CellRead(cell)
            }

            // --- Element/text(text: ..., style: [...]) ---
            ["Element", "text"] => {
                let saved_elem = self.process_element_self_ref(arguments, span);
                let label = self.find_arg_expr(arguments, "text", span);
                let style = self.find_arg_expr_or_default(arguments, "style");

                let cell = self.alloc_cell("text", span);
                let links =
                    self.extract_links_for_element(&self.cells[cell.0 as usize].name.clone());
                let hovered_cell = saved_elem.hovered_cell;

                self.nodes.push(IrNode::Element {
                    cell,
                    kind: ElementKind::Text { label, style },
                    links,
                    hovered_cell,
                });
                self.restore_element_self_ref(saved_elem);
                IrExpr::CellRead(cell)
            }

            // --- Element/slider(element: [...], value: ..., min: ..., max: ..., step: ...) ---
            ["Element", "slider"] => {
                let saved_elem = self.process_element_self_ref(arguments, span);
                let style = self.find_arg_expr_or_default(arguments, "style");
                let value_cell = arguments
                    .iter()
                    .find(|a| a.node.name.as_str() == "value")
                    .and_then(|a| a.node.value.as_ref())
                    .and_then(|v| {
                        let expr = self.lower_expr(&v.node, v.span);
                        match expr {
                            IrExpr::CellRead(c) => Some(c),
                            _ => None,
                        }
                    });
                let min = self.find_arg_number(arguments, "min").unwrap_or(0.0);
                let max = self.find_arg_number(arguments, "max").unwrap_or(100.0);
                let step = self.find_arg_number(arguments, "step").unwrap_or(1.0);

                let cell = self.alloc_cell("slider", span);
                let links =
                    self.extract_links_for_element(&self.cells[cell.0 as usize].name.clone());
                let hovered_cell = saved_elem.hovered_cell;

                self.nodes.push(IrNode::Element {
                    cell,
                    kind: ElementKind::Slider {
                        style,
                        value_cell,
                        min,
                        max,
                        step,
                    },
                    links,
                    hovered_cell,
                });
                self.restore_element_self_ref(saved_elem);
                IrExpr::CellRead(cell)
            }

            // --- Element/select(element: [...], options: LIST { ... }, selected: ...) ---
            ["Element", "select"] => {
                let saved_elem = self.process_element_self_ref(arguments, span);
                let style = self.find_arg_expr_or_default(arguments, "style");
                let options = self.extract_select_options(arguments);
                let selected = arguments
                    .iter()
                    .find(|a| a.node.name.as_str() == "selected")
                    .and_then(|a| a.node.value.as_ref())
                    .map(|v| self.lower_expr(&v.node, v.span));

                let cell = self.alloc_cell("select", span);
                let links =
                    self.extract_links_for_element(&self.cells[cell.0 as usize].name.clone());
                let hovered_cell = saved_elem.hovered_cell;

                self.nodes.push(IrNode::Element {
                    cell,
                    kind: ElementKind::Select {
                        style,
                        options,
                        selected,
                    },
                    links,
                    hovered_cell,
                });
                self.restore_element_self_ref(saved_elem);
                IrExpr::CellRead(cell)
            }

            // --- Element/svg(element: [...], children: ..., style: [...]) ---
            ["Element", "svg"] => {
                let saved_elem = self.process_element_self_ref(arguments, span);
                let style = self.find_arg_expr_or_default(arguments, "style");
                let children_expr = self.find_arg_expr_or_default(arguments, "children");
                let children_cell = self.alloc_cell("svg_children", span);
                self.nodes.push(IrNode::Derived {
                    cell: children_cell,
                    expr: children_expr,
                });

                let cell = self.alloc_cell("svg", span);
                let links =
                    self.extract_links_for_element(&self.cells[cell.0 as usize].name.clone());
                let hovered_cell = saved_elem.hovered_cell;

                self.nodes.push(IrNode::Element {
                    cell,
                    kind: ElementKind::Svg {
                        style,
                        children: children_cell,
                    },
                    links,
                    hovered_cell,
                });
                self.restore_element_self_ref(saved_elem);
                IrExpr::CellRead(cell)
            }

            // --- Element/svg_circle(element: [...], cx: ..., cy: ..., r: ..., style: [...]) ---
            ["Element", "svg_circle"] => {
                let saved_elem = self.process_element_self_ref(arguments, span);
                let cx =
                    self.find_arg_expr_or(arguments, "cx", IrExpr::Constant(IrValue::Number(0.0)));
                let cy =
                    self.find_arg_expr_or(arguments, "cy", IrExpr::Constant(IrValue::Number(0.0)));
                let r =
                    self.find_arg_expr_or(arguments, "r", IrExpr::Constant(IrValue::Number(20.0)));
                let style = self.find_arg_expr_or_default(arguments, "style");

                let cell = self.alloc_cell("svg_circle", span);
                let links =
                    self.extract_links_for_element(&self.cells[cell.0 as usize].name.clone());
                let hovered_cell = saved_elem.hovered_cell;

                self.nodes.push(IrNode::Element {
                    cell,
                    kind: ElementKind::SvgCircle { cx, cy, r, style },
                    links,
                    hovered_cell,
                });
                self.restore_element_self_ref(saved_elem);
                IrExpr::CellRead(cell)
            }

            // --- Element/stripe(direction: ..., items: LIST { ... }, ...) ---
            ["Element", "stripe"] => {
                let saved_elem = self.process_element_self_ref(arguments, span);
                let mut direction = self.find_arg_expr_or_default(arguments, "direction");
                let mut gap =
                    self.find_arg_expr_or(arguments, "gap", IrExpr::Constant(IrValue::Number(0.0)));
                // Fall back to extracting from nested style object
                if matches!(direction, IrExpr::Constant(IrValue::Void)) {
                    if let Some(d) = self.find_field_in_arg_object(arguments, "style", "direction")
                    {
                        direction = d;
                    }
                }
                if matches!(gap, IrExpr::Constant(IrValue::Number(n)) if n == 0.0) {
                    if let Some(g) = self.find_field_in_arg_object(arguments, "style", "gap") {
                        gap = g;
                    }
                }
                let style = self.find_arg_expr_or_default(arguments, "style");
                let element_arg = self.find_arg_expr_or_default(arguments, "element");

                let items_expr = self.find_arg_expr_or_default(arguments, "items");
                let items_cell = self.alloc_cell("stripe_items", span);
                self.nodes.push(IrNode::Derived {
                    cell: items_cell,
                    expr: items_expr,
                });

                let cell = self.alloc_cell("stripe", span);
                let links =
                    self.extract_links_for_element(&self.cells[cell.0 as usize].name.clone());
                let hovered_cell = saved_elem.hovered_cell;

                self.nodes.push(IrNode::Element {
                    cell,
                    kind: ElementKind::Stripe {
                        direction,
                        items: items_cell,
                        gap,
                        style,
                        element_settings: element_arg,
                    },
                    links,
                    hovered_cell,
                });
                self.restore_element_self_ref(saved_elem);
                IrExpr::CellRead(cell)
            }

            // --- Timer/interval(duration: ...) ---
            ["Timer", "interval"] => {
                let duration = self.find_arg_expr(arguments, "duration", span);
                let event = self.alloc_event("timer", EventSource::Timer, span);
                self.nodes.push(IrNode::Timer {
                    event,
                    interval_ms: duration,
                });
                // Timer returns an event source cell.
                let cell = self.alloc_cell("timer_tick", span);
                self.nodes.push(IrNode::Derived {
                    cell,
                    expr: IrExpr::Constant(IrValue::Void),
                });
                // Register this cell as an event source so THEN in pipe chains can find it.
                self.cell_events.insert(cell, event);
                IrExpr::CellRead(cell)
            }

            // --- List/range(from: ..., to: ...) ---
            ["List", "range"] => {
                let from = self.find_arg_expr(arguments, "from", span);
                let to = self.find_arg_expr(arguments, "to", span);
                let from_n = match &from {
                    IrExpr::Constant(IrValue::Number(n)) => Some(*n),
                    IrExpr::CellRead(src) => match self.constant_cells.get(src) {
                        Some(IrValue::Number(n)) => Some(*n),
                        _ => None,
                    },
                    _ => None,
                };
                let to_n = match &to {
                    IrExpr::Constant(IrValue::Number(n)) => Some(*n),
                    IrExpr::CellRead(src) => match self.constant_cells.get(src) {
                        Some(IrValue::Number(n)) => Some(*n),
                        _ => None,
                    },
                    _ => None,
                };
                match (from_n, to_n) {
                    (Some(from_n), Some(to_n)) => {
                        let from_i = from_n as i64;
                        let to_i = to_n as i64;
                        let items = if from_i <= to_i {
                            (from_i..=to_i)
                                .map(|n| IrExpr::Constant(IrValue::Number(n as f64)))
                                .collect()
                        } else {
                            Vec::new()
                        };
                        IrExpr::ListConstruct(items)
                    }
                    _ => {
                        self.error(
                            span,
                            "List/range() currently requires constant `from` and `to` in the Wasm engine",
                        );
                        IrExpr::ListConstruct(Vec::new())
                    }
                }
            }

            // --- Math/sum() (standalone, not in pipe) ---
            ["Math", "sum"] => {
                self.error(
                    span,
                    "Math/sum() must be used in pipe position: `source |> Math/sum()`",
                );
                IrExpr::Constant(IrValue::Void)
            }

            // --- Router/route() ---
            ["Router", "route"] => {
                let event = self.alloc_event("route_change", EventSource::Router, span);
                let cell = self.alloc_cell("route", span);
                self.nodes.push(IrNode::Derived {
                    cell,
                    expr: IrExpr::Constant(IrValue::Text("/".to_string())),
                });
                self.cell_events.insert(cell, event);
                // Route cell is a payload cell: the host updates its value
                // before firing the event, so codegen emits downstream propagation.
                self.events[event.0 as usize].payload_cells.push(cell);
                IrExpr::CellRead(cell)
            }

            // --- Text/empty() ---
            ["Text", "empty"] => IrExpr::Constant(IrValue::Text(String::new())),

            // --- Text/space() ---
            ["Text", "space"] => IrExpr::Constant(IrValue::Text(" ".to_string())),

            // --- Bool/not() — standalone usage (not in pipe) ---
            ["Bool", "not"] => {
                self.error(
                    span,
                    "Bool/not() must be used in pipe position: `value |> Bool/not()`",
                );
                IrExpr::Constant(IrValue::Void)
            }

            // --- Text/trim() — standalone usage ---
            ["Text", "trim"] => {
                self.error(span, "Text/trim() must be used in pipe position");
                IrExpr::Constant(IrValue::Void)
            }

            // --- Text/is_not_empty() — standalone usage ---
            ["Text", "is_not_empty"] => {
                self.error(span, "Text/is_not_empty() must be used in pipe position");
                IrExpr::Constant(IrValue::Void)
            }

            // --- Router/go_to() — standalone usage ---
            ["Router", "go_to"] => {
                self.error(span, "Router/go_to() must be used in pipe position");
                IrExpr::Constant(IrValue::Void)
            }

            // --- User-defined function call ---
            _ => {
                let resolved_fn = if path_strs.len() == 1 {
                    self.resolve_func_name(path_strs[0])
                } else if path_strs.len() == 2 {
                    let qualified = format!("{}/{}", path_strs[0], path_strs[1]);
                    self.resolve_func_name(&qualified)
                } else {
                    None
                };
                if let Some(fn_name) = resolved_fn {
                    let func_def = self.find_func_def(&fn_name).unwrap();
                    let func_id = self.name_to_func[&fn_name];
                    self.current_module = self.function_modules.get(&fn_name).cloned();
                    let result =
                        self.inline_function_call(&fn_name, func_id, &func_def, arguments, span);
                    if let IrExpr::CellRead(result_cell) = result {
                        self.repair_returned_cell_shape(result_cell, span);
                        return IrExpr::CellRead(result_cell);
                    }
                    return result;
                }

                // Unknown — record as CustomCall for later handling.
                let cell = self.alloc_cell("call", span);
                let args: Vec<(String, IrExpr)> = arguments
                    .iter()
                    .map(|a| {
                        let val = a
                            .node
                            .value
                            .as_ref()
                            .map(|v| self.lower_expr(&v.node, v.span))
                            .unwrap_or(IrExpr::Constant(IrValue::Void));
                        (a.node.name.as_str().to_string(), val)
                    })
                    .collect();
                self.nodes.push(IrNode::CustomCall {
                    cell,
                    path: path_strs.iter().map(|s| s.to_string()).collect(),
                    args,
                });
                IrExpr::CellRead(cell)
            }
        }
    }

    // --- Helpers ---

    /// Lower an expression and ensure it has a CellId. If the expression is
    /// already a CellRead, return that cell; otherwise allocate a new derived cell.
    fn lower_expr_to_cell(&mut self, expr: &Spanned<Expression>, name_hint: &str) -> CellId {
        // Check if this is a simple alias → existing cell.
        if let Expression::Alias(alias) = &expr.node {
            if let Some(cell) = self.resolve_alias_cell(alias) {
                return cell;
            }
        }

        let ir_expr = self.lower_expr(&expr.node, expr.span);
        match ir_expr {
            IrExpr::CellRead(cell) => cell,
            _ => {
                let cell = self.alloc_cell(name_hint, expr.span);
                // Record list item constructor when a ListConstruct gets assigned to a cell.
                if matches!(&ir_expr, IrExpr::ListConstruct(_)) {
                    if let Some(constructor_name) = self.pending_list_constructor.take() {
                        self.list_item_constructor.insert(cell, constructor_name);
                    }
                }
                if let Some(fields) = self.extract_list_item_field_exprs_from_expr(&ir_expr) {
                    self.list_item_field_exprs.insert(cell, fields);
                    let _ = self.materialize_list_item_field_cells(cell, cell, expr.span);
                }
                // Track constant values for compile-time WHEN folding.
                if let IrExpr::Constant(ref v) = ir_expr {
                    self.constant_cells.insert(cell, v.clone());
                }
                match &ir_expr {
                    IrExpr::TaggedObject { tag, fields } => {
                        self.constant_cells.insert(cell, IrValue::Tag(tag.clone()));
                        let field_map =
                            self.register_inline_object_field_cells(cell, fields, expr.span);
                        self.cell_field_cells.insert(cell, field_map);
                    }
                    IrExpr::ObjectConstruct(fields) => {
                        let field_map =
                            self.register_inline_object_field_cells(cell, fields, expr.span);
                        self.cell_field_cells.insert(cell, field_map);
                    }
                    _ => {}
                }
                let metadata_source = match &ir_expr {
                    IrExpr::CellRead(src) => Some(*src),
                    _ => self.resolve_field_access_expr_to_cell(&ir_expr),
                };
                self.nodes.push(IrNode::Derived {
                    cell,
                    expr: ir_expr,
                });
                if let Some(src) = metadata_source {
                    if let Some(event) = self.cell_events.get(&src).copied() {
                        self.cell_events.insert(cell, event);
                    }
                    let source_name = self.cells[src.0 as usize].name.clone();
                    if let Some(events) = self.element_events.get(&source_name).cloned() {
                        let alias_name = self.cells[cell.0 as usize].name.clone();
                        self.element_events.insert(alias_name, events);
                    }
                    self.propagate_list_constructor(src, cell);
                }
                cell
            }
        }
    }

    /// Lower a literal to IrValue.
    fn lower_literal(&mut self, lit: &Literal) -> IrValue {
        match lit {
            Literal::Number(n) => IrValue::Number(*n),
            Literal::Tag(s) => {
                let tag = s.as_str();
                // True/False must encode as Bool (0.0/1.0), not as tag table entries,
                // so they match patterns in WHEN/WHILE which use IrPattern::Number(0.0/1.0).
                if tag == "True" {
                    return IrValue::Bool(true);
                }
                if tag == "False" {
                    return IrValue::Bool(false);
                }
                // Intern the tag so it gets a consistent numeric encoding.
                let _encoded = self.intern_tag(tag);
                IrValue::Tag(tag.to_string())
            }
            Literal::Text(s) => IrValue::Text(s.as_str().to_string()),
        }
    }

    /// Lower an alias reference.
    ///
    /// Supports compound name resolution for object-flattened fields:
    /// `elements.item_input.event.key_down.key` tries longest matching prefix
    /// in name_to_cell, then resolves remaining parts via event/field access.
    fn lower_alias(&mut self, alias: &Alias, span: Span) -> IrExpr {
        match alias {
            Alias::WithoutPassed {
                parts,
                referenced_span,
            } => {
                if parts.is_empty() {
                    self.error(span, "Empty alias");
                    return IrExpr::Constant(IrValue::Void);
                }

                if let Some(event_idx) = parts.iter().position(|p| p.as_str() == "event") {
                    if event_idx > 0 && event_idx + 1 < parts.len() {
                        if let Some(resolved_name) =
                            self.resolve_link_alias_through_cells(&parts[..event_idx])
                        {
                            let event_name = parts[event_idx + 1].as_str();
                            if let Some(events) = self.element_events.get(&resolved_name) {
                                if let Some(&event_id) = events.get(event_name) {
                                    if event_idx + 2 == parts.len() {
                                        if let Some(&cell) = self.name_to_cell.get(&resolved_name) {
                                            self.cell_events.insert(cell, event_id);
                                            return IrExpr::CellRead(cell);
                                        }
                                    } else {
                                        let data_name = format!(
                                            "{}.event.{}",
                                            resolved_name,
                                            parts[event_idx + 1..]
                                                .iter()
                                                .map(|p| p.as_str())
                                                .collect::<Vec<_>>()
                                                .join(".")
                                        );
                                        if let Some(&data_cell) = self.name_to_cell.get(&data_name)
                                        {
                                            return IrExpr::CellRead(data_cell);
                                        }
                                    }
                                }
                            }
                            if let Some(resolved_cell) =
                                self.resolve_alias_path_to_cell(&parts[..event_idx])
                            {
                                if let Some(event_id) =
                                    self.find_element_event_for_cell(resolved_cell, event_name)
                                {
                                    if event_idx + 2 == parts.len() {
                                        self.cell_events.insert(resolved_cell, event_id);
                                        return IrExpr::CellRead(resolved_cell);
                                    }
                                    let data_name = format!(
                                        "{}.event.{}",
                                        resolved_name,
                                        parts[event_idx + 1..]
                                            .iter()
                                            .map(|p| p.as_str())
                                            .collect::<Vec<_>>()
                                            .join(".")
                                    );
                                    if let Some(&data_cell) = self.name_to_cell.get(&data_name) {
                                        return IrExpr::CellRead(data_cell);
                                    }
                                }
                            }
                        }
                    }
                }

                let first = parts[0].as_str();

                if parts.len() > 1
                    && let Some(&first_cell) = self.name_to_cell.get(first)
                    && self.resolve_cell_field_cells(first_cell).is_none()
                {
                    let _ = self.materialize_list_item_field_cells(first_cell, first_cell, span);
                    if self.resolve_cell_field_cells(first_cell).is_none()
                        && let Some(nested_fields) = self.resolve_cell_to_inline_object(first_cell)
                    {
                        let inline_map = self.register_inline_object_field_cells(
                            first_cell,
                            &nested_fields,
                            span,
                        );
                        if !inline_map.is_empty() {
                            self.cell_field_cells.insert(first_cell, inline_map);
                        }
                    }
                    if let Some(cell) = self.resolve_alias_path_to_cell(parts) {
                        return IrExpr::CellRead(cell);
                    }
                }

                if parts.len() > 1
                    && let Some(cell) = self.resolve_alias_path_to_cell(parts)
                {
                    return IrExpr::CellRead(cell);
                }

                // Try compound name resolution (longest prefix match).
                // This handles object-flattened paths like "elements.item_input.event.key_down.key".
                if parts.len() > 1 {
                    // Try from longest to shortest compound name.
                    for prefix_len in (2..=parts.len()).rev() {
                        let compound: String = parts[..prefix_len]
                            .iter()
                            .map(|p| p.as_str())
                            .collect::<Vec<_>>()
                            .join(".");
                        if let Some(&cell) = self.name_to_cell.get(&compound) {
                            if prefix_len == parts.len() {
                                return IrExpr::CellRead(cell);
                            }
                            // Remaining parts after the compound match.
                            let remaining = &parts[prefix_len..];
                            // Try resolving the full path using the matched cell's global name.
                            // This handles cases where short alias "elements" maps to cell with
                            // full name "store.elements", and remaining parts form a longer path
                            // like "item_input.event.key_down.key" → "store.elements.item_input.event.key_down.key".
                            let cell_global_name = &self.cells[cell.0 as usize].name;
                            let full_remaining: String = remaining
                                .iter()
                                .map(|p| p.as_str())
                                .collect::<Vec<_>>()
                                .join(".");
                            let global_path = format!("{}.{}", cell_global_name, full_remaining);
                            if let Some(&resolved) = self.name_to_cell.get(&global_path) {
                                return IrExpr::CellRead(resolved);
                            }
                            // Also try partial prefixes of remaining using the global name.
                            for rem_prefix in (1..remaining.len()).rev() {
                                let partial: String = remaining[..rem_prefix]
                                    .iter()
                                    .map(|p| p.as_str())
                                    .collect::<Vec<_>>()
                                    .join(".");
                                let partial_global = format!("{}.{}", cell_global_name, partial);
                                if let Some(&partial_cell) = self.name_to_cell.get(&partial_global)
                                {
                                    let rest = &remaining[rem_prefix..];
                                    // Check for .event pattern on this partial match.
                                    if rest.len() >= 2 && rest[0].as_str() == "event" {
                                        let event_name = rest[1].as_str();
                                        if let Some(events) =
                                            self.element_events.get(&partial_global)
                                        {
                                            if let Some(&event_id) = events.get(event_name) {
                                                if rest.len() == 2 {
                                                    self.cell_events.insert(partial_cell, event_id);
                                                    return IrExpr::CellRead(partial_cell);
                                                }
                                                if rest.len() >= 3 {
                                                    let data_name = format!(
                                                        "{}.event.{}",
                                                        partial_global,
                                                        rest[1..]
                                                            .iter()
                                                            .map(|p| p.as_str())
                                                            .collect::<Vec<_>>()
                                                            .join(".")
                                                    );
                                                    if let Some(&data_cell) =
                                                        self.name_to_cell.get(&data_name)
                                                    {
                                                        return IrExpr::CellRead(data_cell);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    // Check for .text property.
                                    if rest.len() == 1 && rest[0].as_str() == "text" {
                                        let text_name = format!("{}.text", partial_global);
                                        if let Some(&text_cell) = self.name_to_cell.get(&text_name)
                                        {
                                            return IrExpr::CellRead(text_cell);
                                        }
                                    }
                                }
                            }
                            // Check for .event.EVENT_NAME pattern on the matched cell.
                            if remaining.len() >= 2 && remaining[0].as_str() == "event" {
                                let event_name = remaining[1].as_str();
                                // Try to find element events for the compound name.
                                if let Some(events) = self.element_events.get(&compound) {
                                    if let Some(&event_id) = events.get(event_name) {
                                        if remaining.len() == 2 {
                                            // Just the event source — register event and return the cell.
                                            self.cell_events.insert(cell, event_id);
                                            return IrExpr::CellRead(cell);
                                        }
                                        if remaining.len() >= 3 {
                                            // Event payload: .event.key_down.key or .event.change.text
                                            // Build the full data cell name and look it up.
                                            let data_name = format!(
                                                "{}.event.{}",
                                                compound,
                                                remaining[1..]
                                                    .iter()
                                                    .map(|p| p.as_str())
                                                    .collect::<Vec<_>>()
                                                    .join(".")
                                            );
                                            if let Some(&data_cell) =
                                                self.name_to_cell.get(&data_name)
                                            {
                                                return IrExpr::CellRead(data_cell);
                                            }
                                        }
                                    }
                                }
                            }
                            // Check for .text property.
                            if remaining.len() == 1 && remaining[0].as_str() == "text" {
                                let text_name = format!("{}.text", compound);
                                if let Some(&text_cell) = self.name_to_cell.get(&text_name) {
                                    return IrExpr::CellRead(text_cell);
                                }
                            }
                            // Default: build FieldAccess chain for remaining parts.
                            let mut expr = IrExpr::CellRead(cell);
                            for part in remaining {
                                expr = IrExpr::FieldAccess {
                                    object: Box::new(expr),
                                    field: part.as_str().to_string(),
                                };
                            }
                            return expr;
                        }
                    }
                }

                // Simple single-name lookup.
                if let Some(&cell) = self.name_to_cell.get(first) {
                    if parts.len() == 1 {
                        IrExpr::CellRead(cell)
                    } else {
                        // Try resolving the full path using the cell's global name.
                        // Example: "elements" → cell with name "store.elements",
                        // remaining "item_input.event.key_down.key" →
                        // try "store.elements.item_input.event.key_down.key" in name_to_cell.
                        let cell_global_name = &self.cells[cell.0 as usize].name;
                        let remaining_str: String = parts[1..]
                            .iter()
                            .map(|p| p.as_str())
                            .collect::<Vec<_>>()
                            .join(".");
                        let global_path = format!("{}.{}", cell_global_name, remaining_str);
                        if let Some(&resolved) = self.name_to_cell.get(&global_path) {
                            return IrExpr::CellRead(resolved);
                        }
                        // Try partial prefixes of remaining using the global name.
                        let remaining = &parts[1..];
                        for rem_len in (1..remaining.len()).rev() {
                            let partial: String = remaining[..rem_len]
                                .iter()
                                .map(|p| p.as_str())
                                .collect::<Vec<_>>()
                                .join(".");
                            let partial_global = format!("{}.{}", cell_global_name, partial);
                            if let Some(&partial_cell) = self.name_to_cell.get(&partial_global) {
                                let rest = &remaining[rem_len..];
                                // Check for .event pattern.
                                if rest.len() >= 2 && rest[0].as_str() == "event" {
                                    let event_name = rest[1].as_str();
                                    if let Some(events) = self.element_events.get(&partial_global) {
                                        if let Some(&event_id) = events.get(event_name) {
                                            if rest.len() == 2 {
                                                self.cell_events.insert(partial_cell, event_id);
                                                return IrExpr::CellRead(partial_cell);
                                            }
                                            if rest.len() >= 3 {
                                                let data_name = format!(
                                                    "{}.event.{}",
                                                    partial_global,
                                                    rest[1..]
                                                        .iter()
                                                        .map(|p| p.as_str())
                                                        .collect::<Vec<_>>()
                                                        .join(".")
                                                );
                                                if let Some(&data_cell) =
                                                    self.name_to_cell.get(&data_name)
                                                {
                                                    return IrExpr::CellRead(data_cell);
                                                }
                                            }
                                        }
                                    }
                                }
                                // Check for .text property.
                                if rest.len() == 1 && rest[0].as_str() == "text" {
                                    let text_name = format!("{}.text", partial_global);
                                    if let Some(&text_cell) = self.name_to_cell.get(&text_name) {
                                        return IrExpr::CellRead(text_cell);
                                    }
                                }
                            }
                        }
                        // Check for event resolution on the short name.
                        if parts.len() >= 3 && parts[1].as_str() == "event" {
                            let element_name = first;
                            let event_name = parts[2].as_str();
                            if let Some(events) = self.element_events.get(element_name) {
                                if let Some(&event_id) = events.get(event_name) {
                                    if parts.len() == 3 {
                                        self.cell_events.insert(cell, event_id);
                                        return IrExpr::CellRead(cell);
                                    }
                                    if parts.len() >= 4 {
                                        let data_name = format!(
                                            "{}.event.{}",
                                            element_name,
                                            parts[2..]
                                                .iter()
                                                .map(|p| p.as_str())
                                                .collect::<Vec<_>>()
                                                .join(".")
                                        );
                                        if let Some(&data_cell) = self.name_to_cell.get(&data_name)
                                        {
                                            return IrExpr::CellRead(data_cell);
                                        }
                                    }
                                }
                            }
                        }
                        // Dotted access: `foo.bar.baz` — build FieldAccess chain.
                        if self.resolve_cell_field_cells(cell).is_none() {
                            let _ = self.materialize_list_item_field_cells(cell, cell, span);
                            if self.resolve_cell_field_cells(cell).is_none()
                                && let Some(nested_fields) =
                                    self.resolve_cell_to_inline_object(cell)
                            {
                                let inline_map = self.register_inline_object_field_cells(
                                    cell,
                                    &nested_fields,
                                    span,
                                );
                                if !inline_map.is_empty() {
                                    self.cell_field_cells.insert(cell, inline_map);
                                }
                            }
                        }
                        let mut expr = IrExpr::CellRead(cell);
                        for part in &parts[1..] {
                            expr = IrExpr::FieldAccess {
                                object: Box::new(expr),
                                field: part.as_str().to_string(),
                            };
                        }
                        if let Some(resolved) = self.resolve_field_access_expr_to_cell(&expr) {
                            return IrExpr::CellRead(resolved);
                        }
                        expr
                    }
                } else {
                    // Could be a tag or unknown reference.
                    if parts.len() == 1 {
                        // Special-case True/False as booleans (1.0/0.0) for consistency
                        // with Bool/not() and pattern matching.
                        if first == "True" {
                            return IrExpr::Constant(IrValue::Bool(true));
                        }
                        if first == "False" {
                            return IrExpr::Constant(IrValue::Bool(false));
                        }
                        // Treat single unknown names as tags (e.g., `Column`).
                        let _encoded = self.intern_tag(first);
                        IrExpr::Constant(IrValue::Tag(first.to_string()))
                    } else {
                        self.error(span, format!("Unknown variable '{}'", first));
                        IrExpr::Constant(IrValue::Void)
                    }
                }
            }
            Alias::WithPassed { extra_parts } => self.resolve_passed_path(extra_parts, span),
        }
    }

    /// Try to resolve an alias to an existing CellId.
    fn resolve_alias_cell(&self, alias: &Alias) -> Option<CellId> {
        match alias {
            Alias::WithoutPassed { parts, .. } if parts.len() == 1 => {
                self.name_to_cell.get(parts[0].as_str()).copied()
            }
            _ => None,
        }
    }

    /// Pre-resolve a PASS expression that contains `PASSED.x.y` references.
    ///
    /// When a function call uses `PASS: PASSED.theme_options`, the raw expression
    /// is `Alias::WithPassed { extra_parts: ["theme_options"] }`. If stored directly
    /// as `current_passed`, any `PASSED.field` inside the called function would try
    /// to resolve `PASSED.theme_options.field` against the same WithPassed alias —
    /// creating an infinite cycle.
    ///
    /// This function walks the current PASS context object to find the field's
    /// actual expression, breaking the self-referential cycle.
    fn pre_resolve_pass_expr(
        &self,
        expr: &Spanned<Expression>,
        parent_passed: &Option<Spanned<Expression>>,
    ) -> Option<Spanned<Expression>> {
        match &expr.node {
            Expression::Object(obj) => {
                let mut resolved = obj.clone();
                for field in &mut resolved.variables {
                    if let Some(next) = self.pre_resolve_pass_expr(&field.node.value, parent_passed)
                    {
                        field.node.value = next;
                    }
                }
                Some(Spanned {
                    node: Expression::Object(resolved),
                    span: expr.span,
                    persistence: None,
                })
            }
            Expression::Alias(Alias::WithPassed { extra_parts }) => {
                let passed = parent_passed.as_ref()?;
                let mut current = &passed.node;
                let mut current_span = passed.span;
                for part in extra_parts.iter() {
                    match current {
                        Expression::Object(obj) => {
                            let found = obj
                                .variables
                                .iter()
                                .find(|v| v.node.name.as_str() == part.as_str());
                            if let Some(var) = found {
                                current = &var.node.value.node;
                                current_span = var.node.value.span;
                            } else {
                                // Field not found in current PASS object — store as-is.
                                return Some(expr.clone());
                            }
                        }
                        _ => {
                            // Can't walk non-object PASS context — store as-is.
                            return Some(expr.clone());
                        }
                    }
                }
                let resolved = Spanned {
                    node: current.clone(),
                    span: current_span,
                    persistence: None,
                };
                // Recursively resolve if the result is also WithPassed (max 8 levels).
                if matches!(&resolved.node, Expression::Alias(Alias::WithPassed { .. })) {
                    // Save and temporarily replace current_passed for recursive resolution.
                    // (We can't mutate self here since this is &self, so just return as-is
                    // if still WithPassed — the depth guard will catch true cycles.)
                    Some(resolved)
                } else {
                    Some(resolved)
                }
            }
            _ => Some(expr.clone()),
        }
    }

    /// Resolve a `PASSED.x.y.z` path through the current PASSED context.
    ///
    /// The PASSED context is an AST expression (typically an object literal).
    /// We walk through it field by field to resolve the path.
    fn resolve_passed_path(
        &mut self,
        extra_parts: &[crate::parser::StrSlice],
        span: Span,
    ) -> IrExpr {
        // Guard against infinite recursion (lower_alias ↔ resolve_passed_path cycle).
        self.passed_resolve_depth += 1;
        if self.passed_resolve_depth > 32 {
            self.passed_resolve_depth -= 1;
            self.error(span, "PASSED path resolution exceeded maximum depth");
            return IrExpr::Constant(IrValue::Void);
        }
        let result = self.resolve_passed_path_inner(extra_parts, span);
        self.passed_resolve_depth -= 1;
        result
    }

    fn resolve_passed_path_inner(
        &mut self,
        extra_parts: &[crate::parser::StrSlice],
        span: Span,
    ) -> IrExpr {
        let passed = match self.current_passed.clone() {
            Some(p) => p,
            None => {
                self.error(span, "PASSED used but no PASS context is active");
                return IrExpr::Constant(IrValue::Void);
            }
        };

        if extra_parts.is_empty() {
            // Just `PASSED` with no path — lower the whole expression.
            return self.lower_expr(&passed.node, passed.span);
        }

        // Walk through the path: PASSED.a.b.c
        // Start with the PASSED expression, follow each field through object literals.
        let mut current_expr = &passed.node;
        let mut current_span = passed.span;
        let mut remaining_parts: &[crate::parser::StrSlice] = extra_parts;

        while !remaining_parts.is_empty() {
            let field = remaining_parts[0].as_str();
            remaining_parts = &remaining_parts[1..];

            match current_expr {
                Expression::Object(obj) => {
                    // Find the field in the object.
                    let found = obj.variables.iter().find(|v| v.node.name.as_str() == field);
                    if let Some(var) = found {
                        current_expr = &var.node.value.node;
                        current_span = var.node.value.span;
                    } else {
                        // Field not found in object literal — try lowering what we have
                        // and using field access for the rest.
                        let mut expr = self.lower_expr(current_expr, current_span);
                        expr = IrExpr::FieldAccess {
                            object: Box::new(expr),
                            field: field.to_string(),
                        };
                        for part in remaining_parts {
                            expr = IrExpr::FieldAccess {
                                object: Box::new(expr),
                                field: part.as_str().to_string(),
                            };
                        }
                        return expr;
                    }
                }
                Expression::Alias(alias) => {
                    // The current expression is a variable reference — resolve it.
                    // First resolve the alias to get its cell, then try to resolve
                    // remaining parts using the cell's global name in name_to_cell.
                    let base_expr = self.lower_alias(alias, current_span);
                    if let IrExpr::CellRead(cell) = &base_expr {
                        // Build full path: cell's global name + field + remaining parts.
                        let cell_name = &self.cells[cell.0 as usize].name;
                        let mut full_path = format!("{}.{}", cell_name, field);
                        for part in remaining_parts {
                            full_path.push('.');
                            full_path.push_str(part.as_str());
                        }
                        if let Some(&resolved) = self.name_to_cell.get(&full_path) {
                            return IrExpr::CellRead(resolved);
                        }
                        // Try partial prefixes for event/text resolution.
                        let mut all_remaining = vec![field];
                        for part in remaining_parts {
                            all_remaining.push(part.as_str());
                        }
                        for prefix_len in (1..all_remaining.len()).rev() {
                            let partial: String = std::iter::once(cell_name.as_str())
                                .chain(all_remaining[..prefix_len].iter().copied())
                                .collect::<Vec<_>>()
                                .join(".");
                            if let Some(&partial_cell) = self.name_to_cell.get(&partial) {
                                let rest = &all_remaining[prefix_len..];
                                if rest.len() >= 2 && rest[0] == "event" {
                                    if let Some(events) = self.element_events.get(&partial) {
                                        if let Some(&_eid) = events.get(rest[1]) {
                                            if rest.len() == 2 {
                                                return IrExpr::CellRead(partial_cell);
                                            }
                                            let data_name = format!(
                                                "{}.event.{}",
                                                partial,
                                                rest[1..].join(".")
                                            );
                                            if let Some(&data_cell) =
                                                self.name_to_cell.get(&data_name)
                                            {
                                                return IrExpr::CellRead(data_cell);
                                            }
                                        }
                                    }
                                }
                                if rest.len() == 1 && rest[0] == "text" {
                                    let text_name = format!("{}.text", partial);
                                    if let Some(&text_cell) = self.name_to_cell.get(&text_name) {
                                        return IrExpr::CellRead(text_cell);
                                    }
                                }
                            }
                        }
                    }
                    // Fallback: build FieldAccess chain.
                    let mut expr = base_expr;
                    expr = IrExpr::FieldAccess {
                        object: Box::new(expr),
                        field: field.to_string(),
                    };
                    for part in remaining_parts {
                        expr = IrExpr::FieldAccess {
                            object: Box::new(expr),
                            field: part.as_str().to_string(),
                        };
                    }
                    return expr;
                }
                _ => {
                    // For other expression types, lower and use field access.
                    let mut expr = self.lower_expr(current_expr, current_span);
                    expr = IrExpr::FieldAccess {
                        object: Box::new(expr),
                        field: field.to_string(),
                    };
                    for part in remaining_parts {
                        expr = IrExpr::FieldAccess {
                            object: Box::new(expr),
                            field: part.as_str().to_string(),
                        };
                    }
                    return expr;
                }
            }
        }

        // We consumed all parts by walking through object literals.
        self.lower_expr(current_expr, current_span)
    }

    /// Resolve a PASSED path from text interpolation (string parts, not StrSlice).
    fn resolve_passed_text_interp(&mut self, parts: &[&str], span: Span) -> IrExpr {
        let passed = match self.current_passed.clone() {
            Some(p) => p,
            None => {
                self.error(
                    span,
                    "PASSED used in text interpolation but no PASS context is active",
                );
                return IrExpr::Constant(IrValue::Void);
            }
        };

        if parts.is_empty() {
            return self.lower_expr(&passed.node, passed.span);
        }

        // Walk through the path similar to resolve_passed_path.
        let mut current_expr = &passed.node;
        let mut current_span = passed.span;
        let mut remaining_parts: &[&str] = parts;

        while !remaining_parts.is_empty() {
            let field = remaining_parts[0];
            remaining_parts = &remaining_parts[1..];

            match current_expr {
                Expression::Object(obj) => {
                    let found = obj.variables.iter().find(|v| v.node.name.as_str() == field);
                    if let Some(var) = found {
                        current_expr = &var.node.value.node;
                        current_span = var.node.value.span;
                    } else {
                        let mut expr = self.lower_expr(current_expr, current_span);
                        expr = IrExpr::FieldAccess {
                            object: Box::new(expr),
                            field: field.to_string(),
                        };
                        for part in remaining_parts {
                            expr = IrExpr::FieldAccess {
                                object: Box::new(expr),
                                field: part.to_string(),
                            };
                        }
                        return expr;
                    }
                }
                Expression::Alias(alias) => {
                    let mut expr = self.lower_alias(alias, current_span);
                    expr = IrExpr::FieldAccess {
                        object: Box::new(expr),
                        field: field.to_string(),
                    };
                    for part in remaining_parts {
                        expr = IrExpr::FieldAccess {
                            object: Box::new(expr),
                            field: part.to_string(),
                        };
                    }
                    return expr;
                }
                _ => {
                    let mut expr = self.lower_expr(current_expr, current_span);
                    expr = IrExpr::FieldAccess {
                        object: Box::new(expr),
                        field: field.to_string(),
                    };
                    for part in remaining_parts {
                        expr = IrExpr::FieldAccess {
                            object: Box::new(expr),
                            field: part.to_string(),
                        };
                    }
                    return expr;
                }
            }
        }

        self.lower_expr(current_expr, current_span)
    }

    /// Lower arithmetic operators.
    fn lower_arithmetic(&mut self, op: &ArithmeticOperator) -> IrExpr {
        match op {
            ArithmeticOperator::Negate { operand } => {
                IrExpr::UnaryNeg(Box::new(self.lower_expr(&operand.node, operand.span)))
            }
            ArithmeticOperator::Add {
                operand_a,
                operand_b,
            } => IrExpr::BinOp {
                op: BinOp::Add,
                lhs: Box::new(self.lower_expr(&operand_a.node, operand_a.span)),
                rhs: Box::new(self.lower_expr(&operand_b.node, operand_b.span)),
            },
            ArithmeticOperator::Subtract {
                operand_a,
                operand_b,
            } => IrExpr::BinOp {
                op: BinOp::Sub,
                lhs: Box::new(self.lower_expr(&operand_a.node, operand_a.span)),
                rhs: Box::new(self.lower_expr(&operand_b.node, operand_b.span)),
            },
            ArithmeticOperator::Multiply {
                operand_a,
                operand_b,
            } => IrExpr::BinOp {
                op: BinOp::Mul,
                lhs: Box::new(self.lower_expr(&operand_a.node, operand_a.span)),
                rhs: Box::new(self.lower_expr(&operand_b.node, operand_b.span)),
            },
            ArithmeticOperator::Divide {
                operand_a,
                operand_b,
            } => IrExpr::BinOp {
                op: BinOp::Div,
                lhs: Box::new(self.lower_expr(&operand_a.node, operand_a.span)),
                rhs: Box::new(self.lower_expr(&operand_b.node, operand_b.span)),
            },
        }
    }

    /// Lower comparators.
    fn lower_comparator(&mut self, cmp: &Comparator) -> IrExpr {
        match cmp {
            Comparator::Equal {
                operand_a,
                operand_b,
            } => IrExpr::Compare {
                op: CmpOp::Eq,
                lhs: Box::new(self.lower_expr(&operand_a.node, operand_a.span)),
                rhs: Box::new(self.lower_expr(&operand_b.node, operand_b.span)),
            },
            Comparator::NotEqual {
                operand_a,
                operand_b,
            } => IrExpr::Compare {
                op: CmpOp::Ne,
                lhs: Box::new(self.lower_expr(&operand_a.node, operand_a.span)),
                rhs: Box::new(self.lower_expr(&operand_b.node, operand_b.span)),
            },
            Comparator::Greater {
                operand_a,
                operand_b,
            } => IrExpr::Compare {
                op: CmpOp::Gt,
                lhs: Box::new(self.lower_expr(&operand_a.node, operand_a.span)),
                rhs: Box::new(self.lower_expr(&operand_b.node, operand_b.span)),
            },
            Comparator::GreaterOrEqual {
                operand_a,
                operand_b,
            } => IrExpr::Compare {
                op: CmpOp::Ge,
                lhs: Box::new(self.lower_expr(&operand_a.node, operand_a.span)),
                rhs: Box::new(self.lower_expr(&operand_b.node, operand_b.span)),
            },
            Comparator::Less {
                operand_a,
                operand_b,
            } => IrExpr::Compare {
                op: CmpOp::Lt,
                lhs: Box::new(self.lower_expr(&operand_a.node, operand_a.span)),
                rhs: Box::new(self.lower_expr(&operand_b.node, operand_b.span)),
            },
            Comparator::LessOrEqual {
                operand_a,
                operand_b,
            } => IrExpr::Compare {
                op: CmpOp::Le,
                lhs: Box::new(self.lower_expr(&operand_a.node, operand_a.span)),
                rhs: Box::new(self.lower_expr(&operand_b.node, operand_b.span)),
            },
        }
    }

    /// Inline a user-defined function call by binding parameters to argument cells
    /// and lowering the body in the extended scope.
    fn inline_function_call(
        &mut self,
        fn_name: &str,
        func_id: FuncId,
        func_def: &FuncDef,
        arguments: &[Spanned<Argument>],
        span: Span,
    ) -> IrExpr {
        if self
            .active_function_calls
            .iter()
            .any(|name| name == fn_name)
        {
            return self.build_function_call_expr(func_id, func_def, None, arguments, span);
        }

        self.active_function_calls.push(fn_name.to_string());
        self.inline_depth += 1;
        if self.inline_depth > 64 {
            self.inline_depth -= 1;
            self.active_function_calls.pop();
            self.error(
                span,
                "Function inlining exceeded maximum depth (possible recursion)",
            );
            return IrExpr::Constant(IrValue::Void);
        }

        // Save entire name_to_cell state so BLOCK variables created during body
        // lowering don't leak into the caller's scope. Without this, a function
        // like `text()` that inlines `font()` (which also has a `small_base` BLOCK
        // variable) would see font's `small_base` overwrite text's `small_base`.
        let use_full_name_snapshot = self.function_requires_full_name_snapshot(fn_name, func_def);
        let use_prefixed_snapshot = !use_full_name_snapshot
            && self.function_requires_prefixed_name_snapshot(fn_name, func_def);
        let saved_names = use_full_name_snapshot.then(|| self.name_to_cell.clone());
        let saved_prefixed_names =
            use_prefixed_snapshot.then(|| self.capture_prefixed_name_bindings(&func_def.params));
        let saved_exact_names = (!use_full_name_snapshot && !use_prefixed_snapshot)
            .then(|| self.capture_exact_name_bindings(&func_def.params));

        // Save and set module context (caller sets self.current_module before calling).
        let saved_module = self.current_module.clone();

        // Handle PASS argument: set current_passed context.
        // Pre-resolve WithPassed references to break PASSED ↔ resolve_passed_path cycles.
        let saved_passed = self.current_passed.take();
        if let Some(pass_arg) = arguments.iter().find(|a| a.node.name.as_str() == "PASS") {
            if let Some(ref val) = pass_arg.node.value {
                self.current_passed = self.pre_resolve_pass_expr(val, &saved_passed);
            }
        } else {
            // Propagate existing PASSED context to nested calls.
            self.current_passed = saved_passed.clone();
        }

        // Bind named parameters to argument values.
        for (i, param_name) in func_def.params.iter().enumerate() {
            // Try to find the argument by name first, then by position.
            // Skip PASS — it's not a regular parameter.
            let arg_expr = arguments
                .iter()
                .find(|a| a.node.name.as_str() == param_name && a.node.name.as_str() != "PASS")
                .or_else(|| {
                    // By position: skip PASS arguments.
                    let non_pass: Vec<_> = arguments
                        .iter()
                        .filter(|a| a.node.name.as_str() != "PASS")
                        .collect();
                    non_pass.get(i).copied()
                })
                .and_then(|a| a.node.value.as_ref());

            if let Some(val) = arg_expr {
                let cell = self.alloc_cell(param_name, span);
                let mut expr = self.lower_expr(&val.node, val.span);
                let list_constructor = matches!(&expr, IrExpr::ListConstruct(_))
                    .then(|| self.pending_list_constructor.take())
                    .flatten();
                let list_item_fields = self.extract_list_item_field_exprs_from_expr(&expr);
                // Track constant values for compile-time WHEN/WHILE folding.
                // If the argument expression is a constant (e.g., Tag("Material")),
                // or reads from a cell that is known constant, propagate it.
                // TaggedObject: the tag name is constant even when fields are
                // dynamic (e.g., ButtonIcon[checked: some_var]).
                let constant_value = match &expr {
                    IrExpr::Constant(v) => Some(v.clone()),
                    IrExpr::CellRead(src) => self.constant_cells.get(src).cloned(),
                    IrExpr::TaggedObject { tag, .. } => Some(IrValue::Tag(tag.clone())),
                    _ => None,
                };

                // For TaggedObject expressions, create sub-cells for each field
                // and register in cell_field_cells. This enables WHEN pattern
                // destructuring to bind field variables (e.g., `checked` in
                // `ButtonIcon[checked] => ...`).
                let object_field_cells = match &expr {
                    IrExpr::TaggedObject { fields, .. } | IrExpr::ObjectConstruct(fields) => {
                        Some(self.register_inline_object_field_cells(cell, fields, span))
                    }
                    _ => None,
                };

                // If the argument resolves to a namespace cell (object with field
                // cells), propagate field cell names under the parameter name
                // prefix so `param.field` paths resolve correctly (e.g.
                // `todo.title`). This must also work for resolved FieldAccess
                // expressions such as `row_data.cells |> List/get(...)`, not only
                // for plain CellRead arguments.
                if let IrExpr::FieldAccess { object, .. } = &expr
                    && let IrExpr::CellRead(object_cell) = object.as_ref()
                    && self.resolve_cell_field_cells(*object_cell).is_none()
                {
                    let _ =
                        self.materialize_list_item_field_cells(*object_cell, *object_cell, span);
                    if self.resolve_cell_field_cells(*object_cell).is_none()
                        && let Some(nested_fields) =
                            self.resolve_cell_to_inline_object(*object_cell)
                    {
                        let inline_map = self.register_inline_object_field_cells(
                            *object_cell,
                            &nested_fields,
                            span,
                        );
                        if !inline_map.is_empty() {
                            self.cell_field_cells.insert(*object_cell, inline_map);
                        }
                    }
                }
                let namespace_source = match &expr {
                    IrExpr::CellRead(source_cell) => Some(*source_cell),
                    _ => self.resolve_field_access_expr_to_cell(&expr),
                };
                if !matches!(expr, IrExpr::CellRead(_))
                    && let Some(source_cell) = namespace_source
                {
                    expr = IrExpr::CellRead(source_cell);
                }
                if let IrExpr::CellRead(source_cell) = expr.clone()
                    && object_field_cells.is_none()
                {
                    let param_source = self.canonicalize_shape_source_cell(source_cell);
                    self.name_to_cell.insert(param_name.clone(), param_source);
                    let resolved_fields =
                        self.resolve_cell_field_cells(param_source).or_else(|| {
                            self.find_metadata_source_cell(param_source)
                                .and_then(|src| self.resolve_cell_field_cells(src))
                        });
                    if let Some(fields) = resolved_fields {
                        for (field_name, field_cell) in &fields {
                            let dotted = format!("{}.{}", param_name, field_name);
                            self.name_to_cell.insert(dotted, *field_cell);
                        }
                    } else if let Some(nested_fields) =
                        self.resolve_cell_to_inline_object(param_source)
                    {
                        let inline_map = self.register_inline_object_field_cells(
                            param_source,
                            &nested_fields,
                            span,
                        );
                        for (field_name, field_cell) in &inline_map {
                            let dotted = format!("{}.{}", param_name, field_name);
                            self.name_to_cell.insert(dotted, *field_cell);
                        }
                    } else if self.materialize_list_item_field_cells(
                        param_source,
                        param_source,
                        span,
                    ) && let Some(field_map) =
                        self.cell_field_cells.get(&param_source).cloned()
                    {
                        for (field_name, field_cell) in &field_map {
                            let dotted = format!("{}.{}", param_name, field_name);
                            self.name_to_cell.insert(dotted, *field_cell);
                        }
                    }
                    self.propagate_list_constructor(param_source, param_source);
                    continue;
                }
                self.nodes.push(IrNode::Derived { cell, expr });
                self.name_to_cell.insert(param_name.clone(), cell);

                if let Some(constructor) = list_constructor {
                    self.list_item_constructor.insert(cell, constructor);
                }
                if let Some(fields) = list_item_fields {
                    self.list_item_field_exprs.insert(cell, fields);
                    let _ = self.materialize_list_item_field_cells(cell, cell, span);
                }

                if let Some(cv) = constant_value {
                    self.constant_cells.insert(cell, cv);
                }

                if let Some(field_map) = object_field_cells {
                    for (field_name, field_cell) in &field_map {
                        let dotted = format!("{}.{}", param_name, field_name);
                        self.name_to_cell.insert(dotted, *field_cell);
                    }
                    self.cell_field_cells.insert(cell, field_map);
                } else if let Some(source_cell) = namespace_source {
                    let resolved_fields =
                        self.resolve_cell_field_cells(source_cell).or_else(|| {
                            self.find_metadata_source_cell(source_cell)
                                .and_then(|src| self.resolve_cell_field_cells(src))
                        });
                    if let Some(fields) = resolved_fields {
                        for (field_name, field_cell) in &fields {
                            let dotted = format!("{}.{}", param_name, field_name);
                            self.name_to_cell.insert(dotted, *field_cell);
                        }
                        self.cell_field_cells.insert(cell, fields);
                    } else if let Some(nested_fields) =
                        self.resolve_cell_to_inline_object(source_cell)
                    {
                        let inline_map =
                            self.register_inline_object_field_cells(cell, &nested_fields, span);
                        for (field_name, field_cell) in &inline_map {
                            let dotted = format!("{}.{}", param_name, field_name);
                            self.name_to_cell.insert(dotted, *field_cell);
                        }
                        if !inline_map.is_empty() {
                            self.cell_field_cells.insert(cell, inline_map);
                        }
                    }
                    if !self.cell_field_cells.contains_key(&cell)
                        && self.materialize_list_item_field_cells(cell, source_cell, span)
                        && let Some(field_map) = self.cell_field_cells.get(&cell).cloned()
                    {
                        for (field_name, field_cell) in &field_map {
                            let dotted = format!("{}.{}", param_name, field_name);
                            self.name_to_cell.insert(dotted, *field_cell);
                        }
                    }
                    self.propagate_list_constructor(source_cell, cell);
                }
            }
        }

        // Lower the function body.
        let result = self.lower_expr(&func_def.body.node, func_def.body.span);
        if let IrExpr::CellRead(result_cell) = result {
            self.repair_returned_cell_shape(result_cell, span);
        }

        // Restore entire name_to_cell state (undoes both parameter bindings
        // and any BLOCK variables created during body lowering).
        if let Some(saved_names) = saved_names {
            self.name_to_cell = saved_names;
        } else if let Some(saved_prefixed_names) = saved_prefixed_names {
            self.restore_prefixed_name_bindings(&func_def.params, saved_prefixed_names);
        } else if let Some(saved_exact_names) = saved_exact_names {
            self.restore_exact_name_bindings(saved_exact_names);
        }

        // Restore previous PASSED context.
        self.current_passed = saved_passed;
        // Restore previous module context.
        self.current_module = saved_module;

        self.inline_depth -= 1;
        self.active_function_calls.pop();
        result
    }

    /// Inline a user-defined function call with a piped argument.
    /// The piped value becomes the first parameter.
    /// `outer_var_name` is used to propagate LINK events from the outer variable
    /// to elements created inside the function body.
    fn inline_function_call_with_pipe(
        &mut self,
        fn_name: &str,
        func_id: FuncId,
        func_def: &FuncDef,
        pipe_source: CellId,
        arguments: &[Spanned<Argument>],
        span: Span,
    ) -> IrExpr {
        if self
            .active_function_calls
            .iter()
            .any(|name| name == fn_name)
        {
            return self.build_function_call_expr(
                func_id,
                func_def,
                Some(IrExpr::CellRead(pipe_source)),
                arguments,
                span,
            );
        }

        self.active_function_calls.push(fn_name.to_string());
        self.inline_depth += 1;
        if self.inline_depth > 64 {
            self.inline_depth -= 1;
            self.active_function_calls.pop();
            self.error(
                span,
                "Function inlining exceeded maximum depth (possible recursion)",
            );
            return IrExpr::Constant(IrValue::Void);
        }

        // Save entire name_to_cell state so BLOCK variables created during body
        // lowering don't leak into the caller's scope (same rationale as
        // inline_function_call).
        let use_full_name_snapshot = self.function_requires_full_name_snapshot(fn_name, func_def);
        let use_prefixed_snapshot = !use_full_name_snapshot
            && self.function_requires_prefixed_name_snapshot(fn_name, func_def);
        let saved_names = use_full_name_snapshot.then(|| self.name_to_cell.clone());
        let saved_prefixed_names =
            use_prefixed_snapshot.then(|| self.capture_prefixed_name_bindings(&func_def.params));
        let saved_exact_names = (!use_full_name_snapshot && !use_prefixed_snapshot)
            .then(|| self.capture_exact_name_bindings(&func_def.params));

        // Save module context (caller sets self.current_module before calling).
        let saved_module = self.current_module.clone();

        // Handle PASS argument: set current_passed context.
        // Pre-resolve WithPassed references to break PASSED ↔ resolve_passed_path cycles.
        let saved_passed = self.current_passed.take();
        if let Some(pass_arg) = arguments.iter().find(|a| a.node.name.as_str() == "PASS") {
            if let Some(ref val) = pass_arg.node.value {
                self.current_passed = self.pre_resolve_pass_expr(val, &saved_passed);
            }
        } else {
            // Propagate existing PASSED context to nested calls.
            self.current_passed = saved_passed.clone();
        }

        // First param is the piped value.
        if let Some(first_param) = func_def.params.first() {
            let param_source = self.canonicalize_shape_source_cell(pipe_source);
            self.name_to_cell.insert(first_param.clone(), param_source);

            // If the piped source is a namespace cell, propagate field cell
            // names so `param.field` paths resolve (e.g., `todo.title`).
            if let Some(fields) = self.resolve_cell_field_cells(param_source) {
                for (field_name, field_cell) in &fields {
                    let dotted = format!("{}.{}", first_param, field_name);
                    self.name_to_cell.insert(dotted, *field_cell);
                }
            } else if let Some(nested_fields) = self.resolve_cell_to_inline_object(param_source) {
                let inline_map =
                    self.register_inline_object_field_cells(param_source, &nested_fields, span);
                for (field_name, field_cell) in &inline_map {
                    let dotted = format!("{}.{}", first_param, field_name);
                    self.name_to_cell.insert(dotted, *field_cell);
                }
            } else {
                let param_cell = self.alloc_cell(first_param, span);
                self.nodes.push(IrNode::Derived {
                    cell: param_cell,
                    expr: IrExpr::CellRead(param_source),
                });
                self.name_to_cell.insert(first_param.clone(), param_cell);
                if self.materialize_list_item_field_cells(param_cell, param_source, span) {
                    if let Some(fields) = self.cell_field_cells.get(&param_cell).cloned() {
                        for (field_name, field_cell) in &fields {
                            let dotted = format!("{}.{}", first_param, field_name);
                            self.name_to_cell.insert(dotted, *field_cell);
                        }
                    }
                }
            }
        }

        // Bind remaining named parameters to argument values.
        for (i, param_name) in func_def.params.iter().enumerate().skip(1) {
            let arg_expr = arguments
                .iter()
                .find(|a| a.node.name.as_str() == param_name && a.node.name.as_str() != "PASS")
                .or_else(|| {
                    let non_pass: Vec<_> = arguments
                        .iter()
                        .filter(|a| a.node.name.as_str() != "PASS")
                        .collect();
                    non_pass.get(i.saturating_sub(1)).copied()
                })
                .and_then(|a| a.node.value.as_ref());

            if let Some(val) = arg_expr {
                let cell = self.alloc_cell(param_name, span);
                let mut expr = self.lower_expr(&val.node, val.span);
                let list_constructor = matches!(&expr, IrExpr::ListConstruct(_))
                    .then(|| self.pending_list_constructor.take())
                    .flatten();
                let list_item_fields = self.extract_list_item_field_exprs_from_expr(&expr);

                // Track constant values for compile-time WHEN/WHILE folding.
                // TaggedObject: tag is constant even when fields are dynamic.
                let constant_value = match &expr {
                    IrExpr::Constant(v) => Some(v.clone()),
                    IrExpr::CellRead(src) => self.constant_cells.get(src).cloned(),
                    IrExpr::TaggedObject { tag, .. } => Some(IrValue::Tag(tag.clone())),
                    _ => None,
                };

                // For TaggedObject, create sub-cells for field destructuring.
                let object_field_cells = match &expr {
                    IrExpr::TaggedObject { fields, .. } | IrExpr::ObjectConstruct(fields) => {
                        Some(self.register_inline_object_field_cells(cell, fields, span))
                    }
                    _ => None,
                };

                // Extract namespace source BEFORE moving expr into Derived node.
                if let IrExpr::FieldAccess { object, .. } = &expr
                    && let IrExpr::CellRead(object_cell) = object.as_ref()
                    && self.resolve_cell_field_cells(*object_cell).is_none()
                {
                    let _ =
                        self.materialize_list_item_field_cells(*object_cell, *object_cell, span);
                    if self.resolve_cell_field_cells(*object_cell).is_none()
                        && let Some(nested_fields) =
                            self.resolve_cell_to_inline_object(*object_cell)
                    {
                        let inline_map = self.register_inline_object_field_cells(
                            *object_cell,
                            &nested_fields,
                            span,
                        );
                        if !inline_map.is_empty() {
                            self.cell_field_cells.insert(*object_cell, inline_map);
                        }
                    }
                }
                let namespace_source = match &expr {
                    IrExpr::CellRead(source_cell) => Some(*source_cell),
                    _ => self.resolve_field_access_expr_to_cell(&expr),
                };
                if !matches!(expr, IrExpr::CellRead(_))
                    && let Some(source_cell) = namespace_source
                {
                    expr = IrExpr::CellRead(source_cell);
                }
                if let IrExpr::CellRead(source_cell) = expr.clone()
                    && object_field_cells.is_none()
                {
                    let param_source = self.canonicalize_shape_source_cell(source_cell);
                    self.name_to_cell.insert(param_name.clone(), param_source);
                    let resolved_fields =
                        self.resolve_cell_field_cells(param_source).or_else(|| {
                            self.find_metadata_source_cell(param_source)
                                .and_then(|src| self.resolve_cell_field_cells(src))
                        });
                    if let Some(fields) = resolved_fields {
                        for (field_name, field_cell) in &fields {
                            let dotted = format!("{}.{}", param_name, field_name);
                            self.name_to_cell.insert(dotted, *field_cell);
                        }
                    } else if let Some(nested_fields) =
                        self.resolve_cell_to_inline_object(param_source)
                    {
                        let inline_map = self.register_inline_object_field_cells(
                            param_source,
                            &nested_fields,
                            span,
                        );
                        for (field_name, field_cell) in &inline_map {
                            let dotted = format!("{}.{}", param_name, field_name);
                            self.name_to_cell.insert(dotted, *field_cell);
                        }
                    } else if self.materialize_list_item_field_cells(
                        param_source,
                        param_source,
                        span,
                    ) && let Some(field_map) =
                        self.cell_field_cells.get(&param_source).cloned()
                    {
                        for (field_name, field_cell) in &field_map {
                            let dotted = format!("{}.{}", param_name, field_name);
                            self.name_to_cell.insert(dotted, *field_cell);
                        }
                    }
                    self.propagate_list_constructor(param_source, param_source);
                    continue;
                }
                self.nodes.push(IrNode::Derived { cell, expr });
                self.name_to_cell.insert(param_name.clone(), cell);

                if let Some(constructor) = list_constructor {
                    self.list_item_constructor.insert(cell, constructor);
                }
                if let Some(fields) = list_item_fields {
                    self.list_item_field_exprs.insert(cell, fields);
                    let _ = self.materialize_list_item_field_cells(cell, cell, span);
                }

                if let Some(cv) = constant_value {
                    self.constant_cells.insert(cell, cv);
                }

                // Propagate namespace field cell names (same as inline_function_call).
                if let Some(field_map) = object_field_cells {
                    for (field_name, field_cell) in &field_map {
                        let dotted = format!("{}.{}", param_name, field_name);
                        self.name_to_cell.insert(dotted, *field_cell);
                    }
                    self.cell_field_cells.insert(cell, field_map);
                } else if let Some(source_cell) = namespace_source {
                    let resolved_fields =
                        self.resolve_cell_field_cells(source_cell).or_else(|| {
                            self.find_metadata_source_cell(source_cell)
                                .and_then(|src| self.resolve_cell_field_cells(src))
                        });
                    if let Some(fields) = resolved_fields {
                        for (field_name, field_cell) in &fields {
                            let dotted = format!("{}.{}", param_name, field_name);
                            self.name_to_cell.insert(dotted, *field_cell);
                        }
                        self.cell_field_cells.insert(cell, fields);
                    } else if let Some(nested_fields) =
                        self.resolve_cell_to_inline_object(source_cell)
                    {
                        let inline_map =
                            self.register_inline_object_field_cells(cell, &nested_fields, span);
                        for (field_name, field_cell) in &inline_map {
                            let dotted = format!("{}.{}", param_name, field_name);
                            self.name_to_cell.insert(dotted, *field_cell);
                        }
                        if !inline_map.is_empty() {
                            self.cell_field_cells.insert(cell, inline_map);
                        }
                    }
                    if !self.cell_field_cells.contains_key(&cell)
                        && self.materialize_list_item_field_cells(cell, source_cell, span)
                        && let Some(field_map) = self.cell_field_cells.get(&cell).cloned()
                    {
                        for (field_name, field_cell) in &field_map {
                            let dotted = format!("{}.{}", param_name, field_name);
                            self.name_to_cell.insert(dotted, *field_cell);
                        }
                    }
                    self.propagate_list_constructor(source_cell, cell);
                }
            }
        }

        // Lower the function body.
        let result = self.lower_expr(&func_def.body.node, func_def.body.span);
        if let IrExpr::CellRead(result_cell) = result {
            self.repair_returned_cell_shape(result_cell, span);
        }

        // Restore entire name_to_cell state (undoes both parameter bindings
        // and any BLOCK variables created during body lowering).
        if let Some(saved_names) = saved_names {
            self.name_to_cell = saved_names;
        } else if let Some(saved_prefixed_names) = saved_prefixed_names {
            self.restore_prefixed_name_bindings(&func_def.params, saved_prefixed_names);
        } else if let Some(saved_exact_names) = saved_exact_names {
            self.restore_exact_name_bindings(saved_exact_names);
        }

        // Restore previous PASSED context.
        self.current_passed = saved_passed;
        // Restore previous module context.
        self.current_module = saved_module;

        self.inline_depth -= 1;
        self.active_function_calls.pop();
        result
    }

    fn build_function_call_expr(
        &mut self,
        func_id: FuncId,
        func_def: &FuncDef,
        pipe_source: Option<IrExpr>,
        arguments: &[Spanned<Argument>],
        span: Span,
    ) -> IrExpr {
        let mut args = Vec::with_capacity(func_def.params.len());

        for (i, param_name) in func_def.params.iter().enumerate() {
            let arg_expr = if i == 0 {
                if let Some(pipe_expr) = pipe_source.clone() {
                    Some(pipe_expr)
                } else {
                    arguments
                        .iter()
                        .find(|a| {
                            a.node.name.as_str() == param_name && a.node.name.as_str() != "PASS"
                        })
                        .or_else(|| {
                            let non_pass: Vec<_> = arguments
                                .iter()
                                .filter(|a| a.node.name.as_str() != "PASS")
                                .collect();
                            non_pass.get(i).copied()
                        })
                        .and_then(|a| a.node.value.as_ref())
                        .map(|val| self.lower_expr(&val.node, val.span))
                }
            } else {
                let positional_index = if pipe_source.is_some() { i - 1 } else { i };
                arguments
                    .iter()
                    .find(|a| a.node.name.as_str() == param_name && a.node.name.as_str() != "PASS")
                    .or_else(|| {
                        let non_pass: Vec<_> = arguments
                            .iter()
                            .filter(|a| a.node.name.as_str() != "PASS")
                            .collect();
                        non_pass.get(positional_index).copied()
                    })
                    .and_then(|a| a.node.value.as_ref())
                    .map(|val| self.lower_expr(&val.node, val.span))
            };

            args.push(arg_expr.unwrap_or(IrExpr::Constant(IrValue::Void)));
        }

        if args.is_empty() && pipe_source.is_some() && func_def.params.is_empty() {
            self.error(span, "Piped function call has no parameters");
        }

        IrExpr::FunctionCall {
            func: func_id,
            args,
        }
    }

    /// Lower a pattern. Tags in patterns are interned in the tag table.
    fn lower_pattern(&mut self, pattern: &crate::parser::static_expression::Pattern) -> IrPattern {
        use crate::parser::static_expression::Pattern;
        match pattern {
            Pattern::Literal(Literal::Number(n)) => IrPattern::Number(*n),
            Pattern::Literal(Literal::Text(s)) => IrPattern::Text(s.as_str().to_string()),
            Pattern::Literal(Literal::Tag(s)) => {
                let tag = s.as_str();
                // True/False tags match boolean-encoded values (1.0/0.0).
                if tag == "True" {
                    IrPattern::Number(1.0)
                } else if tag == "False" {
                    IrPattern::Number(0.0)
                } else {
                    // Intern the tag so codegen can find its encoded value.
                    self.intern_tag(tag);
                    IrPattern::Tag(tag.to_string())
                }
            }
            Pattern::WildCard => IrPattern::Wildcard,
            Pattern::Alias { name } => IrPattern::Binding(name.as_str().to_string()),
            // Complex patterns (Object, List, Map, TaggedObject) → treat as wildcard for now.
            _ => IrPattern::Wildcard,
        }
    }

    /// Check if an AST pattern matches a known constant IrValue.
    /// Used for compile-time WHEN folding: when the source is a known constant,
    /// only the matching arm needs to be lowered.
    /// Distribute a WHEN that produces objects into per-field WHEN cells.
    ///
    /// When ALL non-SKIP arms produce objects (ObjectConstruct or CellRead of
    /// object-store cells), this creates an object-store parent cell (`target`)
    /// with per-field WHEN cells. Returns true if distribution was performed.
    ///
    /// Example: `theme |> WHEN { A => [x: 1, y: 2], B => [x: 3, y: 4] }`
    /// becomes: `target.x = theme |> WHEN { A => 1, B => 3 }`
    ///          `target.y = theme |> WHEN { A => 2, B => 4 }`
    /// Follow Derived(CellRead) and PipeThrough chains to find the underlying
    /// `cell_field_cells` entry. Returns a clone of the field map if found.
    fn resolve_cell_field_cells(&self, cell: CellId) -> Option<HashMap<String, CellId>> {
        self.resolve_cell_field_cells_inner(cell, &mut HashSet::new())
    }

    fn resolve_cell_field_cells_inner(
        &self,
        cell: CellId,
        seen: &mut HashSet<CellId>,
    ) -> Option<HashMap<String, CellId>> {
        if !seen.insert(cell) {
            return None;
        }
        if let Some(fields) = self.cell_field_cells.get(&cell)
            && !fields.is_empty()
        {
            let result = Some(fields.clone());
            seen.remove(&cell);
            return result;
        }
        if let Some(field_exprs) = self.list_item_field_exprs.get(&cell) {
            let mut field_map = HashMap::new();
            for (field_name, field_expr) in field_exprs {
                if let Some(field_cell) = self.resolve_field_access_expr_to_cell(field_expr) {
                    field_map.insert(field_name.clone(), field_cell);
                }
            }
            if !field_map.is_empty() {
                let result = Some(field_map);
                seen.remove(&cell);
                return result;
            }
        }
        // Follow the chain through Derived(CellRead), Derived(FieldAccess), and PipeThrough nodes.
        let mut result = None;
        for node in self.nodes.iter().rev() {
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
                        } else if let Some(field_cell) =
                            self.resolve_field_access_expr_to_cell(field_expr)
                        {
                            field_map.insert(field_name.clone(), field_cell);
                        }
                    }
                    if !field_map.is_empty() {
                        result = Some(field_map);
                        break;
                    }
                }
                IrNode::Derived {
                    cell: c,
                    expr: IrExpr::CellRead(src),
                } if *c == cell => {
                    result = self.resolve_cell_field_cells_inner(*src, seen);
                    break;
                }
                IrNode::Derived {
                    cell: c,
                    expr: IrExpr::FieldAccess { object, field },
                } if *c == cell => {
                    // Follow FieldAccess: resolve the object's field cells, find the field,
                    // and recursively resolve the target field cell's sub-fields.
                    if let IrExpr::CellRead(obj_cell) = object.as_ref() {
                        if let Some(obj_fields) =
                            self.resolve_cell_field_cells_inner(*obj_cell, seen)
                        {
                            if let Some(&field_cell) = obj_fields.get(field.as_str()) {
                                result = self.resolve_cell_field_cells_inner(field_cell, seen);
                                break;
                            }
                        }
                    }
                    result = None;
                    break;
                }
                IrNode::PipeThrough { cell: c, source } if *c == cell => {
                    result = self.resolve_cell_field_cells_inner(*source, seen);
                    break;
                }
                _ => {}
            }
        }
        seen.remove(&cell);
        result
    }

    fn propagate_expr_field_cells(&mut self, target: CellId, expr: &IrExpr) {
        match expr {
            IrExpr::ObjectConstruct(fields) => {
                let alias_fields = self.register_inline_object_field_cells(
                    target,
                    fields,
                    self.cells[target.0 as usize].span,
                );
                if !alias_fields.is_empty() {
                    self.cell_field_cells.insert(target, alias_fields);
                }
                return;
            }
            IrExpr::TaggedObject { fields, .. } => {
                let alias_fields = self.register_inline_object_field_cells(
                    target,
                    fields,
                    self.cells[target.0 as usize].span,
                );
                if !alias_fields.is_empty() {
                    self.cell_field_cells.insert(target, alias_fields);
                }
                return;
            }
            _ => {}
        }

        let Some(source_fields) = ({
            match expr {
                IrExpr::CellRead(source) => self.resolve_cell_field_cells(*source),
                IrExpr::FieldAccess { object, field } => {
                    if let IrExpr::CellRead(object_cell) = object.as_ref() {
                        self.resolve_cell_field_cells(*object_cell)
                            .and_then(|object_fields| {
                                object_fields.get(field).copied().and_then(|field_cell| {
                                    self.resolve_cell_field_cells(field_cell)
                                })
                            })
                    } else {
                        None
                    }
                }
                _ => None,
            }
        }) else {
            return;
        };

        let alias_fields = self.build_field_alias_map(target, &source_fields);
        if !alias_fields.is_empty() {
            self.cell_field_cells.insert(target, alias_fields);
        }
    }

    fn build_field_alias_map(
        &mut self,
        target: CellId,
        source_fields: &HashMap<String, CellId>,
    ) -> HashMap<String, CellId> {
        let target_name = self.cells[target.0 as usize].name.clone();
        let span = self.cells[target.0 as usize].span;
        let mut alias_fields = HashMap::new();
        for (field_name, source_field) in source_fields {
            let canonical_source = if self.find_list_constructor(*source_field).is_some()
                || self.find_list_item_field_exprs(*source_field).is_some()
            {
                self.canonicalize_shape_source_cell(
                    self.canonicalize_representative_cell(*source_field),
                )
            } else {
                self.canonicalize_shape_source_cell(*source_field)
            };
            let alias_name = format!("{}.{}", target_name, field_name);
            let alias_cell = self.alloc_cell(&alias_name, span);
            self.nodes.push(IrNode::Derived {
                cell: alias_cell,
                expr: IrExpr::CellRead(canonical_source),
            });
            if let Some(event) = self.cell_events.get(&canonical_source).copied() {
                self.cell_events.insert(alias_cell, event);
            }
            let source_field_name = self.cells[canonical_source.0 as usize].name.clone();
            if let Some(events) = self.element_events.get(&source_field_name).cloned() {
                self.element_events.insert(alias_name.clone(), events);
            }
            if let Some(constructor) = self.find_list_constructor(canonical_source) {
                self.list_item_constructor.insert(alias_cell, constructor);
            }
            if let Some(field_exprs) = self.find_list_item_field_exprs(canonical_source) {
                self.list_item_field_exprs.insert(alias_cell, field_exprs);
            }
            if (self.find_list_constructor(canonical_source).is_some()
                || self.find_list_item_field_exprs(canonical_source).is_some())
                && self.cell_field_cells.get(&alias_cell).is_none()
            {
                let _ = self.materialize_list_item_field_cells(alias_cell, canonical_source, span);
            }
            if let Some(nested_source_fields) = self.resolve_cell_field_cells(canonical_source) {
                let nested_alias_fields =
                    self.build_field_alias_map(alias_cell, &nested_source_fields);
                if !nested_alias_fields.is_empty() {
                    self.cell_field_cells
                        .insert(alias_cell, nested_alias_fields);
                }
            } else if let Some(nested_fields) = self.resolve_cell_to_inline_object(canonical_source)
            {
                let nested_alias_fields =
                    self.register_inline_object_field_cells(alias_cell, &nested_fields, span);
                if !nested_alias_fields.is_empty() {
                    self.cell_field_cells
                        .insert(alias_cell, nested_alias_fields);
                }
            }
            alias_fields.insert(field_name.clone(), alias_cell);
        }
        alias_fields
    }

    fn register_inline_object_field_cells(
        &mut self,
        parent_cell: CellId,
        fields: &[(String, IrExpr)],
        span: Span,
    ) -> HashMap<String, CellId> {
        if let Some(existing) = self.cell_field_cells.get(&parent_cell)
            && !existing.is_empty()
        {
            return existing.clone();
        }
        let parent_name = self.cells[parent_cell.0 as usize].name.clone();
        let mut field_map = HashMap::new();

        for (field_name, field_expr) in fields {
            let dotted = format!("{}.{}", parent_name, field_name);
            let lowered_field_expr = match field_expr {
                IrExpr::CellRead(source_cell) => {
                    IrExpr::CellRead(self.canonicalize_representative_cell(*source_cell))
                }
                _ => field_expr.clone(),
            };
            let field_cell = if let Some(&existing_cell) = self.name_to_cell.get(&dotted) {
                existing_cell
            } else {
                let field_cell = self.alloc_cell(&dotted, span);
                self.nodes.push(IrNode::Derived {
                    cell: field_cell,
                    expr: lowered_field_expr.clone(),
                });
                self.name_to_cell.insert(dotted.clone(), field_cell);
                field_cell
            };

            let metadata_source = match &lowered_field_expr {
                IrExpr::CellRead(source_cell) => {
                    Some(self.canonicalize_shape_source_cell(*source_cell))
                }
                _ => self
                    .resolve_field_access_expr_to_cell(&lowered_field_expr)
                    .map(|source_cell| self.canonicalize_shape_source_cell(source_cell)),
            };

            if let Some(source_cell) = metadata_source {
                if let Some(event) = self.cell_events.get(&source_cell).copied() {
                    self.cell_events.insert(field_cell, event);
                }
                let source_field_name = self.cells[source_cell.0 as usize].name.clone();
                if let Some(events) = self.element_events.get(&source_field_name).cloned() {
                    let alias_name = self.cells[field_cell.0 as usize].name.clone();
                    self.element_events.insert(alias_name, events);
                }
                if let Some(source_fields) = self.resolve_cell_field_cells(source_cell) {
                    let alias_fields = self.build_field_alias_map(field_cell, &source_fields);
                    if !alias_fields.is_empty() {
                        self.cell_field_cells.insert(field_cell, alias_fields);
                    }
                } else if let Some(nested_fields) = self.resolve_cell_to_inline_object(source_cell)
                {
                    let nested_map =
                        self.register_inline_object_field_cells(field_cell, &nested_fields, span);
                    if !nested_map.is_empty() {
                        self.cell_field_cells.insert(field_cell, nested_map);
                    }
                } else if let Some(field_exprs) = self.find_list_item_field_exprs(source_cell) {
                    let mut inline_fields: Vec<_> = field_exprs
                        .iter()
                        .map(|(name, expr)| {
                            (
                                name.clone(),
                                self.inline_cell_reads_in_expr(expr.clone(), &mut HashSet::new()),
                            )
                        })
                        .collect();
                    inline_fields.sort_by(|(left, _), (right, _)| left.cmp(right));
                    let nested_map =
                        self.register_inline_object_field_cells(field_cell, &inline_fields, span);
                    if !nested_map.is_empty() {
                        self.cell_field_cells.insert(field_cell, nested_map);
                    }
                }
                if let Some(constructor) = self.find_list_constructor(source_cell) {
                    self.list_item_constructor.insert(field_cell, constructor);
                }
                if let Some(field_exprs) = self.find_list_item_field_exprs(source_cell) {
                    self.list_item_field_exprs.insert(field_cell, field_exprs);
                }
                if (self.find_list_constructor(source_cell).is_some()
                    || self.find_list_item_field_exprs(source_cell).is_some())
                    && self.cell_field_cells.get(&field_cell).is_none()
                {
                    let _ = self.materialize_list_item_field_cells(field_cell, source_cell, span);
                }
            }

            match &lowered_field_expr {
                IrExpr::ObjectConstruct(nested_fields)
                | IrExpr::TaggedObject {
                    fields: nested_fields,
                    ..
                } => {
                    let nested_map =
                        self.register_inline_object_field_cells(field_cell, nested_fields, span);
                    if !nested_map.is_empty() {
                        self.cell_field_cells.insert(field_cell, nested_map);
                    }
                }
                _ => {}
            }

            field_map.insert(field_name.clone(), field_cell);
        }

        field_map
    }

    /// Follow CellRead/Derived/PipeThrough chains to find an inline object or
    /// TaggedObject expression. Returns the fields if found.
    fn resolve_cell_to_inline_object(&self, cell: CellId) -> Option<Vec<(String, IrExpr)>> {
        self.resolve_cell_to_inline_object_inner(cell, &mut HashSet::new())
    }

    fn resolve_cell_to_inline_object_inner(
        &self,
        cell: CellId,
        visiting: &mut HashSet<CellId>,
    ) -> Option<Vec<(String, IrExpr)>> {
        if !visiting.insert(cell) {
            return None;
        }
        let result = self.nodes.iter().rev().find_map(|node| match node {
            IrNode::Derived {
                cell: c,
                expr: IrExpr::ObjectConstruct(fields),
            } if *c == cell => Some(
                fields
                    .iter()
                    .map(|(name, expr)| (name.clone(), self.reduce_representative_expr(expr)))
                    .collect(),
            ),
            IrNode::Derived {
                cell: c,
                expr: IrExpr::TaggedObject { fields, .. },
            } if *c == cell => Some(
                fields
                    .iter()
                    .map(|(name, expr)| (name.clone(), self.reduce_representative_expr(expr)))
                    .collect(),
            ),
            IrNode::Derived {
                cell: c,
                expr: IrExpr::CellRead(src),
            } if *c == cell => self.resolve_cell_to_inline_object_inner(*src, visiting),
            IrNode::Derived {
                cell: c,
                expr: IrExpr::FieldAccess { object, field },
            } if *c == cell => {
                if let IrExpr::CellRead(obj_cell) = object.as_ref()
                    && let Some(obj_fields) = self.resolve_cell_field_cells(*obj_cell)
                    && let Some(&field_cell) = obj_fields.get(field.as_str())
                {
                    return self.resolve_cell_to_inline_object_inner(field_cell, visiting);
                }
                None
            }
            IrNode::PipeThrough { cell: c, source } if *c == cell => {
                self.resolve_cell_to_inline_object_inner(*source, visiting)
            }
            _ => None,
        });
        visiting.remove(&cell);
        result
    }

    /// If a cell is a WHEN/WHILE with object-valued arms (TaggedObject/ObjectConstruct),
    /// distribute it into component sub-cells with cell_field_cells. This allows outer
    /// distributions to resolve CellRead(cell) into field references.
    ///
    /// Example: a WHEN cell with Oklch tagged object arms gets distributed into
    /// lightness/chroma/hue sub-cells, each with their own WHEN over the same source.
    fn ensure_cell_distributed(&mut self, cell: CellId) -> bool {
        // Already distributed?
        if self.cell_field_cells.contains_key(&cell) {
            return true;
        }

        // Find WHEN/WHILE node for this cell and extract its source + arms.
        let node_info = self.nodes.iter().find_map(|n| match n {
            IrNode::When {
                cell: c,
                source,
                arms,
            } if *c == cell => Some((*source, arms.clone())),
            IrNode::While {
                cell: c,
                source,
                arms,
                ..
            } if *c == cell => Some((*source, arms.clone())),
            _ => None,
        });

        let (source, arms) = match node_info {
            Some(info) => info,
            None => return false,
        };

        // Check if any non-skip arm has distributable object bodies.
        let has_object_arms = arms.iter().any(|(_, body)| {
            matches!(
                body,
                IrExpr::TaggedObject { .. } | IrExpr::ObjectConstruct(_)
            )
        });
        if !has_object_arms {
            return false;
        }

        // Distribute using existing logic. This creates sub-cells, registers
        // cell_field_cells, and pushes a Derived(Void) node for the parent.
        // The original WHEN/WHILE node remains but is effectively superseded —
        // consumers use cell_field_cells to access component values.
        self.try_distribute_when_object(cell, source, &arms)
    }

    fn try_distribute_when_object(
        &mut self,
        target: CellId,
        source: CellId,
        arms: &[(IrPattern, IrExpr)],
    ) -> bool {
        // Extract fields from each arm body. Skip SKIP arms.
        let mut all_arm_fields: Vec<Option<Vec<(String, IrExpr)>>> = Vec::new();
        let mut field_names_ordered: Vec<String> = Vec::new();
        let mut field_set = std::collections::HashSet::new();

        for (idx, (pattern, body)) in arms.iter().enumerate() {
            if matches!(
                body,
                IrExpr::Constant(IrValue::Skip) | IrExpr::Constant(IrValue::Void)
            ) {
                all_arm_fields.push(None);
                continue;
            }
            let fields = match body {
                IrExpr::ObjectConstruct(fields) => fields.clone(),
                IrExpr::TaggedObject { fields, .. } => fields.clone(),
                IrExpr::FieldAccess { object, field } => {
                    // Try to resolve FieldAccess(CellRead(c), "f") → cell_field_cells[c]["f"]
                    if let IrExpr::CellRead(obj_cell) = object.as_ref() {
                        if let Some(field_map) = self.resolve_cell_field_cells(*obj_cell) {
                            if let Some(&field_cell) = field_map.get(field.as_str()) {
                                // Successfully resolved — treat as CellRead of the field cell.
                                let resolved_body = IrExpr::CellRead(field_cell);
                                if let Some(inner_fields) =
                                    self.resolve_cell_field_cells(field_cell)
                                {
                                    let mut fields = Vec::new();
                                    for (name, fc) in &inner_fields {
                                        let expr = if let Some(node) = self.nodes.iter().find(|n| {
                                            matches!(n, IrNode::Derived { cell: c, .. } if *c == *fc)
                                        }) {
                                            match node {
                                                IrNode::Derived { expr, .. } => {
                                                    if matches!(expr, IrExpr::Constant(IrValue::Void))
                                                        && self.cell_field_cells.contains_key(fc)
                                                    {
                                                        IrExpr::CellRead(*fc)
                                                    } else {
                                                        expr.clone()
                                                    }
                                                }
                                                _ => IrExpr::CellRead(*fc),
                                            }
                                        } else {
                                            IrExpr::CellRead(*fc)
                                        };
                                        fields.push((name.clone(), expr));
                                    }
                                    fields
                                } else if let Some(inline) =
                                    self.resolve_cell_to_inline_object(field_cell)
                                {
                                    inline
                                } else if self.ensure_cell_distributed(field_cell) {
                                    if let Some(sub_fields) =
                                        self.resolve_cell_field_cells(field_cell)
                                    {
                                        let mut fields = Vec::new();
                                        for (name, fc) in &sub_fields {
                                            fields.push((name.clone(), IrExpr::CellRead(*fc)));
                                        }
                                        fields
                                    } else {
                                        return false;
                                    }
                                } else {
                                    return false;
                                }
                            } else {
                                return false;
                            }
                        } else {
                            return false;
                        }
                    } else {
                        return false;
                    }
                }
                IrExpr::CellRead(cell) => {
                    // Use resolve_cell_field_cells to follow Derived/PipeThrough chains.
                    if let Some(field_map) = self.resolve_cell_field_cells(*cell) {
                        // Reconstruct fields from object-store.
                        let mut fields = Vec::new();
                        for (name, field_cell) in &field_map {
                            let expr = if let Some(node) = self.nodes.iter().find(|n| {
                                matches!(n, IrNode::Derived { cell: c, .. } if *c == *field_cell)
                            }) {
                                match node {
                                    IrNode::Derived { expr, .. } => {
                                        // If this field is itself a namespace cell
                                        // (Derived(Void) with cell_field_cells), return
                                        // CellRead to preserve the link to sub-fields.
                                        // Otherwise the raw Constant(Void) would lose the
                                        // connection and cause all arms to look identical.
                                        if matches!(expr, IrExpr::Constant(IrValue::Void))
                                            && self.cell_field_cells.contains_key(field_cell)
                                        {
                                            IrExpr::CellRead(*field_cell)
                                        } else {
                                            expr.clone()
                                        }
                                    }
                                    _ => IrExpr::CellRead(*field_cell),
                                }
                            } else {
                                IrExpr::CellRead(*field_cell)
                            };
                            fields.push((name.clone(), expr));
                        }
                        fields
                    } else if let Some(inline) = self.resolve_cell_to_inline_object(*cell) {
                        // Cell resolved to an inline object (e.g., TaggedObject from Oklch).
                        inline
                    } else if self.ensure_cell_distributed(*cell) {
                        // The cell was a WHEN/WHILE with object arms (e.g., Oklch).
                        // It's now distributed into component sub-cells.
                        if let Some(field_map) = self.resolve_cell_field_cells(*cell) {
                            let mut fields = Vec::new();
                            for (name, field_cell) in &field_map {
                                fields.push((name.clone(), IrExpr::CellRead(*field_cell)));
                            }
                            fields
                        } else {
                            return false;
                        }
                    } else {
                        return false; // Not an object-store cell
                    }
                }
                _ => {
                    return false; // Not an object
                }
            };
            for (name, _) in &fields {
                if field_set.insert(name.clone()) {
                    field_names_ordered.push(name.clone());
                }
            }
            all_arm_fields.push(Some(fields));
        }

        // Need at least one non-SKIP arm with fields.
        if field_names_ordered.is_empty() {
            return false;
        }

        // Create a namespace cell for the parent (Void value, just grouping).
        self.nodes.push(IrNode::Derived {
            cell: target,
            expr: IrExpr::Constant(IrValue::Void),
        });

        let target_name = self.cells[target.0 as usize].name.clone();
        let mut field_map = HashMap::new();

        for field_name in &field_names_ordered {
            let field_cell = self.alloc_cell(
                &format!("{}.{}", target_name, field_name),
                self.cells[target.0 as usize].span,
            );

            // Build per-field WHEN arms.
            let field_arms: Vec<(IrPattern, IrExpr)> = arms
                .iter()
                .zip(all_arm_fields.iter())
                .map(|((pattern, _), arm_fields)| {
                    let field_expr = if let Some(fields) = arm_fields {
                        fields
                            .iter()
                            .find(|(n, _)| n == field_name)
                            .map(|(_, e)| e.clone())
                            .unwrap_or(IrExpr::Constant(IrValue::Void))
                    } else {
                        IrExpr::Constant(IrValue::Skip)
                    };
                    (pattern.clone(), field_expr)
                })
                .collect();

            // Check if all non-SKIP arms have the same constant value — fold to Derived.
            let non_skip: Vec<&IrExpr> = field_arms
                .iter()
                .filter(|(_, e)| !matches!(e, IrExpr::Constant(IrValue::Skip)))
                .map(|(_, e)| e)
                .collect();
            let all_same_constant = non_skip.len() > 1
                && non_skip.windows(2).all(|w| {
                    matches!((w[0], w[1]),
                        (IrExpr::Constant(a), IrExpr::Constant(b)) if std::mem::discriminant(a) == std::mem::discriminant(b) && format!("{:?}", a) == format!("{:?}", b)
                    )
                });

            if all_same_constant {
                // All arms agree — emit a simple Derived node.
                self.nodes.push(IrNode::Derived {
                    cell: field_cell,
                    expr: non_skip[0].clone(),
                });
            } else if self.try_distribute_when_object(field_cell, source, &field_arms) {
                // Recursive distribution: this field's WHEN arms are themselves
                // objects (e.g., font: [size: ..., color: ...] per theme).
                // Distributed into per-sub-field WHEN cells.
            } else {
                // Collect CellRead deps from arm bodies — these are inner WHEN/WHILE
                // cells whose values may change independently of the pattern source.
                // Using While with deps ensures re-evaluation when deps change
                // (e.g., dark mode toggle updates inner WHEN cells that the outer
                // distributed WHEN arms read from).
                let mut deps = Vec::new();
                for (_, body) in &field_arms {
                    if let IrExpr::CellRead(c) = body {
                        if *c != source && !deps.contains(c) {
                            deps.push(*c);
                        }
                    }
                }
                if deps.is_empty() {
                    self.nodes.push(IrNode::When {
                        cell: field_cell,
                        source,
                        arms: field_arms,
                    });
                } else {
                    self.nodes.push(IrNode::While {
                        cell: field_cell,
                        source,
                        deps,
                        arms: field_arms,
                    });
                }
            }

            // Register name in name_to_cell for intra-object references.
            let dotted = format!("{}.{}", target_name, field_name);
            self.name_to_cell.insert(dotted, field_cell);
            field_map.insert(field_name.clone(), field_cell);
        }

        self.cell_field_cells.insert(target, field_map);
        true
    }

    fn pattern_matches_constant(
        &self,
        pattern: &crate::parser::static_expression::Pattern,
        value: &IrValue,
    ) -> bool {
        use crate::parser::static_expression::Pattern;
        match (pattern, value) {
            (Pattern::Literal(Literal::Tag(s)), IrValue::Tag(t)) => s.as_str() == t,
            (Pattern::Literal(Literal::Tag(s)), IrValue::Bool(b)) => {
                (s.as_str() == "True" && *b) || (s.as_str() == "False" && !*b)
            }
            (Pattern::Literal(Literal::Number(n)), IrValue::Number(v)) => *n == *v,
            (Pattern::Literal(Literal::Text(s)), IrValue::Text(t)) => s.as_str() == t,
            // TaggedObject pattern matches if the tag names are equal.
            // E.g., pattern `ButtonIcon[checked]` matches value Tag("ButtonIcon").
            (Pattern::TaggedObject { tag, .. }, IrValue::Tag(t)) => tag.as_str() == t,
            (Pattern::WildCard, _) => true,
            (Pattern::Alias { .. }, _) => true,
            _ => false,
        }
    }

    /// Bind destructured fields from a TaggedObject pattern to the source
    /// cell's field cells. Returns saved bindings for later restoration.
    ///
    /// For pattern `ButtonIcon[checked]` with source_cell that has
    /// `cell_field_cells = { "checked": cell_42 }`, this inserts
    /// `name_to_cell["checked"] = cell_42` so the arm body can reference it.
    fn bind_tagged_pattern_fields(
        &mut self,
        pattern: &crate::parser::static_expression::Pattern,
        source_cell: CellId,
    ) -> Vec<(String, Option<CellId>)> {
        use crate::parser::static_expression::Pattern;
        let mut saved = Vec::new();
        if let Pattern::TaggedObject { variables, .. } = pattern {
            if let Some(field_cells) = self.cell_field_cells.get(&source_cell).cloned() {
                for var in variables {
                    let field_name = var.name.as_str();
                    if let Some(&field_cell) = field_cells.get(field_name) {
                        let prev = self.name_to_cell.get(field_name).copied();
                        self.name_to_cell.insert(field_name.to_string(), field_cell);
                        saved.push((field_name.to_string(), prev));
                    }
                }
            }
        }
        saved
    }

    /// Restore name_to_cell bindings that were saved by bind_tagged_pattern_fields.
    fn unbind_pattern_fields(&mut self, saved: Vec<(String, Option<CellId>)>) {
        for (name, prev) in saved {
            if let Some(prev_cell) = prev {
                self.name_to_cell.insert(name, prev_cell);
            } else {
                self.name_to_cell.remove(&name);
            }
        }
    }

    /// Lower a pipe expression to an IrExpr (not emitting a separate node for the target).
    fn lower_pipe_to_expr(
        &mut self,
        from: &Spanned<Expression>,
        to: &Spanned<Expression>,
    ) -> IrExpr {
        // Try to inline simple pipe expressions that can be computed as IrExpr
        // without creating separate nodes. This is critical for HOLD/THEN bodies
        // where the expression must be re-evaluated at event time.
        if let Expression::FunctionCall { path, arguments } = &to.node {
            let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
            let resolved_fn = if path_strs.len() == 1 {
                self.resolve_func_name(path_strs[0])
            } else if path_strs.len() == 2 {
                let qualified = format!("{}/{}", path_strs[0], path_strs[1]);
                self.resolve_func_name(&qualified)
            } else {
                None
            };
            if let Some(fn_name) = resolved_fn {
                let func_def = self.find_func_def(&fn_name).unwrap();
                let func_id = self.name_to_func[&fn_name];
                self.current_module = self.function_modules.get(&fn_name).cloned();
                let source_expr = self.lower_expr(&from.node, from.span);
                let source_cell = match source_expr {
                    IrExpr::CellRead(cell) => cell,
                    other => {
                        let cell = self.alloc_cell("pipe_inline_arg", from.span);
                        self.nodes.push(IrNode::Derived { cell, expr: other });
                        cell
                    }
                };
                return self.inline_function_call_with_pipe(
                    &fn_name,
                    func_id,
                    &func_def,
                    source_cell,
                    arguments,
                    to.span,
                );
            }
            match path_strs.as_slice() {
                ["List", "get"] => {
                    let source = self.lower_expr(&from.node, from.span);
                    let source_cell = match &source {
                        IrExpr::CellRead(cell) => Some(*cell),
                        _ => self.resolve_field_access_expr_to_cell(&source),
                    };
                    if let Some(index) = self.find_constant_index_argument(arguments, to.span) {
                        if let Some(item) = self.try_resolve_list_get_expr(&source, index) {
                            return item;
                        }
                    }
                    let cell = self.alloc_cell("pipe_result", from.span);
                    self.lower_pipe(cell, from, to, from.span);
                    if let Some(source_cell) = source_cell {
                        let metadata_source = self
                            .find_metadata_source_cell(source_cell)
                            .unwrap_or(source_cell);
                        let shape_source = if self.find_list_constructor(source_cell).is_some()
                            || self.find_list_item_field_exprs(source_cell).is_some()
                        {
                            source_cell
                        } else {
                            metadata_source
                        };
                        if std::env::var("BOON_WASM_LIST_GET_TRACE").is_ok() {
                            eprintln!(
                                "[list_get_expr_fallback] target={} source={} metadata={} shape={} ctor(source)={:?} ctor(shape)={:?} fields(source)={} item_fields(source)={} fields(shape)={} item_fields(shape)={}",
                                self.cells[cell.0 as usize].name,
                                self.cells[source_cell.0 as usize].name,
                                self.cells[metadata_source.0 as usize].name,
                                self.cells[shape_source.0 as usize].name,
                                self.find_list_constructor(source_cell),
                                self.find_list_constructor(shape_source),
                                self.cell_field_cells
                                    .get(&source_cell)
                                    .map(|m| m.len())
                                    .unwrap_or(0),
                                self.find_list_item_field_exprs(source_cell)
                                    .map(|m| m.len())
                                    .unwrap_or(0),
                                self.cell_field_cells
                                    .get(&shape_source)
                                    .map(|m| m.len())
                                    .unwrap_or(0),
                                self.find_list_item_field_exprs(shape_source)
                                    .map(|m| m.len())
                                    .unwrap_or(0),
                            );
                        }
                        self.materialize_list_item_field_cells(cell, shape_source, from.span);
                        self.propagate_list_constructor(shape_source, cell);
                    }
                    return IrExpr::CellRead(cell);
                }
                ["Bool", "not"] => {
                    let source = self.lower_expr(&from.node, from.span);
                    return IrExpr::Not(Box::new(source));
                }
                _ => {}
            }
        }
        // Allocate an anonymous cell for the pipe result.
        let cell = self.alloc_cell("pipe_result", from.span);
        self.lower_pipe(cell, from, to, from.span);
        IrExpr::CellRead(cell)
    }

    /// Find a named argument and return its lowered IrExpr.
    fn find_arg_expr(&mut self, arguments: &[Spanned<Argument>], name: &str, span: Span) -> IrExpr {
        if let Some(arg) = arguments.iter().find(|a| a.node.name.as_str() == name) {
            if let Some(ref val) = arg.node.value {
                return self.lower_expr(&val.node, val.span);
            }
        }
        self.error(span, format!("Missing required argument '{}'", name));
        IrExpr::Constant(IrValue::Void)
    }

    /// Find a named argument or return Void.
    fn find_arg_expr_or_default(&mut self, arguments: &[Spanned<Argument>], name: &str) -> IrExpr {
        if let Some(arg) = arguments.iter().find(|a| a.node.name.as_str() == name) {
            if let Some(ref val) = arg.node.value {
                return self.lower_expr(&val.node, val.span);
            }
        }
        IrExpr::Constant(IrValue::Void)
    }

    fn find_constant_index_argument(
        &mut self,
        arguments: &[Spanned<Argument>],
        span: Span,
    ) -> Option<usize> {
        let index_expr = self.find_arg_expr(arguments, "index", span);
        match index_expr {
            IrExpr::Constant(IrValue::Number(n)) if n >= 1.0 => Some(n as usize),
            IrExpr::CellRead(src) => match self.constant_cells.get(&src) {
                Some(IrValue::Number(n)) if *n >= 1.0 => Some(*n as usize),
                _ => None,
            },
            _ => None,
        }
    }

    fn try_resolve_list_get_expr(&self, source: &IrExpr, index: usize) -> Option<IrExpr> {
        match source {
            IrExpr::ListConstruct(items) => items
                .get(index.saturating_sub(1))
                .cloned()
                .map(|expr| self.inline_cell_reads_in_expr(expr, &mut HashSet::new())),
            IrExpr::CellRead(cell) => {
                self.try_resolve_list_get_from_cell(*cell, index, &mut HashSet::new())
            }
            _ => None,
        }
    }

    fn try_resolve_list_get_from_cell(
        &self,
        cell: CellId,
        index: usize,
        visiting: &mut HashSet<CellId>,
    ) -> Option<IrExpr> {
        if !visiting.insert(cell) {
            return None;
        }

        let has_concrete_item_shape = self.cell_field_cells.contains_key(&cell)
            || self.resolve_cell_to_inline_object(cell).is_some()
            || self.find_list_item_field_exprs(cell).is_some();
        if !has_concrete_item_shape
            && let Some(metadata_source) = self.find_metadata_source_cell(cell)
            && metadata_source != cell
        {
            let result = self.try_resolve_list_get_from_cell(metadata_source, index, visiting);
            if result.is_some() {
                visiting.remove(&cell);
                return result;
            }
        }

        if (self.find_list_constructor(cell).is_some()
            || self.find_list_item_field_exprs(cell).is_some())
            && let Some(fields) = self.cell_field_cells.get(&cell)
        {
            let mut representative_fields: Vec<_> = fields
                .iter()
                .map(|(name, field_cell)| (name.clone(), IrExpr::CellRead(*field_cell)))
                .collect();
            representative_fields.sort_by(|(left, _), (right, _)| left.cmp(right));
            visiting.remove(&cell);
            return Some(IrExpr::ObjectConstruct(representative_fields));
        }

        if let Some(nested_fields) = self.resolve_cell_to_inline_object(cell) {
            visiting.remove(&cell);
            return Some(IrExpr::ObjectConstruct(nested_fields));
        }

        if let Some(fields) = self.list_item_field_exprs.get(&cell) {
            let mut representative_fields: Vec<_> = fields
                .iter()
                .map(|(name, expr)| {
                    (
                        name.clone(),
                        self.inline_cell_reads_in_expr(expr.clone(), &mut HashSet::new()),
                    )
                })
                .collect();
            representative_fields.sort_by(|(left, _), (right, _)| left.cmp(right));
            visiting.remove(&cell);
            return Some(IrExpr::ObjectConstruct(representative_fields));
        }

        let result = self.nodes.iter().rev().find_map(|node| match node {
            IrNode::Derived { cell: c, expr } if *c == cell => match expr {
                IrExpr::ListConstruct(items) => items
                    .get(index.saturating_sub(1))
                    .cloned()
                    .map(|expr| self.inline_cell_reads_in_expr(expr, &mut HashSet::new())),
                IrExpr::CellRead(src) => self.try_resolve_list_get_from_cell(*src, index, visiting),
                IrExpr::FieldAccess { .. } => self
                    .resolve_field_access_expr_to_cell(expr)
                    .and_then(|field_cell| {
                        self.try_resolve_list_get_from_cell(field_cell, index, visiting)
                    }),
                _ => None,
            },
            IrNode::PipeThrough { cell: c, source } if *c == cell => {
                self.try_resolve_list_get_from_cell(*source, index, visiting)
            }
            IrNode::ListMap {
                cell: c,
                source,
                item_cell,
                template,
                ..
            } if *c == cell => {
                let item_expr = self.try_resolve_list_get_from_cell(*source, index, visiting)?;
                match template.as_ref() {
                    IrNode::Derived { expr, .. } => {
                        let substituted =
                            self.substitute_expr_cell(expr.clone(), *item_cell, &item_expr);
                        Some(self.inline_cell_reads_in_expr(substituted, &mut HashSet::new()))
                    }
                    _ => None,
                }
            }
            _ => None,
        });

        visiting.remove(&cell);
        result
    }

    fn substitute_expr_cell(&self, expr: IrExpr, target: CellId, replacement: &IrExpr) -> IrExpr {
        match expr {
            IrExpr::CellRead(cell) if cell == target => replacement.clone(),
            IrExpr::CellRead(cell) => IrExpr::CellRead(cell),
            IrExpr::FieldAccess { object, field } => IrExpr::FieldAccess {
                object: Box::new(self.substitute_expr_cell(*object, target, replacement)),
                field,
            },
            IrExpr::BinOp { op, lhs, rhs } => IrExpr::BinOp {
                op,
                lhs: Box::new(self.substitute_expr_cell(*lhs, target, replacement)),
                rhs: Box::new(self.substitute_expr_cell(*rhs, target, replacement)),
            },
            IrExpr::UnaryNeg(inner) => IrExpr::UnaryNeg(Box::new(self.substitute_expr_cell(
                *inner,
                target,
                replacement,
            ))),
            IrExpr::Compare { op, lhs, rhs } => IrExpr::Compare {
                op,
                lhs: Box::new(self.substitute_expr_cell(*lhs, target, replacement)),
                rhs: Box::new(self.substitute_expr_cell(*rhs, target, replacement)),
            },
            IrExpr::TextConcat(parts) => IrExpr::TextConcat(
                parts
                    .into_iter()
                    .map(|part| match part {
                        TextSegment::Literal(text) => TextSegment::Literal(text),
                        TextSegment::Expr(expr) => {
                            TextSegment::Expr(self.substitute_expr_cell(expr, target, replacement))
                        }
                    })
                    .collect(),
            ),
            IrExpr::FunctionCall { func, args } => IrExpr::FunctionCall {
                func,
                args: args
                    .into_iter()
                    .map(|arg| self.substitute_expr_cell(arg, target, replacement))
                    .collect(),
            },
            IrExpr::Not(inner) => IrExpr::Not(Box::new(self.substitute_expr_cell(
                *inner,
                target,
                replacement,
            ))),
            IrExpr::ObjectConstruct(fields) => IrExpr::ObjectConstruct(
                fields
                    .into_iter()
                    .map(|(name, value)| {
                        (name, self.substitute_expr_cell(value, target, replacement))
                    })
                    .collect(),
            ),
            IrExpr::ListConstruct(items) => IrExpr::ListConstruct(
                items
                    .into_iter()
                    .map(|item| self.substitute_expr_cell(item, target, replacement))
                    .collect(),
            ),
            IrExpr::TaggedObject { tag, fields } => IrExpr::TaggedObject {
                tag,
                fields: fields
                    .into_iter()
                    .map(|(name, value)| {
                        (name, self.substitute_expr_cell(value, target, replacement))
                    })
                    .collect(),
            },
            IrExpr::PatternMatch { source, arms } => IrExpr::PatternMatch {
                source,
                arms: arms
                    .into_iter()
                    .map(|(pattern, body)| {
                        (
                            pattern,
                            self.substitute_expr_cell(body, target, replacement),
                        )
                    })
                    .collect(),
            },
            IrExpr::Constant(value) => IrExpr::Constant(value),
        }
    }

    fn inline_cell_reads_in_expr(&self, expr: IrExpr, visiting: &mut HashSet<CellId>) -> IrExpr {
        match expr {
            IrExpr::CellRead(cell) => {
                if !visiting.insert(cell) {
                    return IrExpr::CellRead(cell);
                }
                let is_event_payload = self
                    .events
                    .iter()
                    .any(|event| event.payload_cells.contains(&cell));
                let resolved = self
                    .nodes
                    .iter()
                    .find_map(|node| match node {
                        IrNode::Derived { cell: c, expr } if *c == cell => Some(
                            if is_event_payload
                                || self.cell_field_cells.contains_key(&cell)
                                || self.list_item_constructor.contains_key(&cell)
                                || self.list_item_field_exprs.contains_key(&cell)
                                || (matches!(expr, IrExpr::Constant(IrValue::Void))
                                    && self.cell_field_cells.contains_key(&cell))
                            {
                                IrExpr::CellRead(cell)
                            } else {
                                self.inline_cell_reads_in_expr(expr.clone(), visiting)
                            },
                        ),
                        IrNode::PipeThrough { cell: c, source } if *c == cell => Some(
                            self.inline_cell_reads_in_expr(IrExpr::CellRead(*source), visiting),
                        ),
                        _ => None,
                    })
                    .unwrap_or(IrExpr::CellRead(cell));
                visiting.remove(&cell);
                resolved
            }
            IrExpr::FieldAccess { object, field } => IrExpr::FieldAccess {
                object: Box::new(self.inline_cell_reads_in_expr(*object, visiting)),
                field,
            },
            IrExpr::BinOp { op, lhs, rhs } => IrExpr::BinOp {
                op,
                lhs: Box::new(self.inline_cell_reads_in_expr(*lhs, visiting)),
                rhs: Box::new(self.inline_cell_reads_in_expr(*rhs, visiting)),
            },
            IrExpr::UnaryNeg(inner) => {
                IrExpr::UnaryNeg(Box::new(self.inline_cell_reads_in_expr(*inner, visiting)))
            }
            IrExpr::Compare { op, lhs, rhs } => IrExpr::Compare {
                op,
                lhs: Box::new(self.inline_cell_reads_in_expr(*lhs, visiting)),
                rhs: Box::new(self.inline_cell_reads_in_expr(*rhs, visiting)),
            },
            IrExpr::TextConcat(parts) => IrExpr::TextConcat(
                parts
                    .into_iter()
                    .map(|part| match part {
                        TextSegment::Literal(text) => TextSegment::Literal(text),
                        TextSegment::Expr(expr) => {
                            TextSegment::Expr(self.inline_cell_reads_in_expr(expr, visiting))
                        }
                    })
                    .collect(),
            ),
            IrExpr::FunctionCall { func, args } => IrExpr::FunctionCall {
                func,
                args: args
                    .into_iter()
                    .map(|arg| self.inline_cell_reads_in_expr(arg, visiting))
                    .collect(),
            },
            IrExpr::Not(inner) => {
                IrExpr::Not(Box::new(self.inline_cell_reads_in_expr(*inner, visiting)))
            }
            IrExpr::ObjectConstruct(fields) => IrExpr::ObjectConstruct(
                fields
                    .into_iter()
                    .map(|(name, value)| (name, self.inline_cell_reads_in_expr(value, visiting)))
                    .collect(),
            ),
            IrExpr::ListConstruct(items) => IrExpr::ListConstruct(
                items
                    .into_iter()
                    .map(|item| self.inline_cell_reads_in_expr(item, visiting))
                    .collect(),
            ),
            IrExpr::TaggedObject { tag, fields } => IrExpr::TaggedObject {
                tag,
                fields: fields
                    .into_iter()
                    .map(|(name, value)| (name, self.inline_cell_reads_in_expr(value, visiting)))
                    .collect(),
            },
            IrExpr::PatternMatch { source, arms } => IrExpr::PatternMatch {
                source,
                arms: arms
                    .into_iter()
                    .map(|(pattern, body)| {
                        (pattern, self.inline_cell_reads_in_expr(body, visiting))
                    })
                    .collect(),
            },
            IrExpr::Constant(value) => IrExpr::Constant(value),
        }
    }

    /// Extract a field from within a named argument's object value.
    /// Used when fields like `direction` or `gap` are nested inside `style: [direction: Right, ...]`.
    fn find_field_in_arg_object(
        &mut self,
        arguments: &[Spanned<Argument>],
        arg_name: &str,
        field_name: &str,
    ) -> Option<IrExpr> {
        let arg = arguments
            .iter()
            .find(|a| a.node.name.as_str() == arg_name)?;
        let val = arg.node.value.as_ref()?;
        if let Expression::Object(obj) = &val.node {
            for field in &obj.variables {
                if field.node.name.as_str() == field_name {
                    return Some(self.lower_expr(&field.node.value.node, field.node.value.span));
                }
            }
        }
        None
    }

    /// Find a named argument or return a default.
    fn find_arg_expr_or(
        &mut self,
        arguments: &[Spanned<Argument>],
        name: &str,
        default: IrExpr,
    ) -> IrExpr {
        if let Some(arg) = arguments.iter().find(|a| a.node.name.as_str() == name) {
            if let Some(ref val) = arg.node.value {
                return self.lower_expr(&val.node, val.span);
            }
        }
        default
    }

    /// Extract a numeric literal from a named argument.
    fn find_arg_number(&self, arguments: &[Spanned<Argument>], name: &str) -> Option<f64> {
        let arg = arguments.iter().find(|a| a.node.name.as_str() == name)?;
        let val = arg.node.value.as_ref()?;
        match &val.node {
            Expression::Literal(Literal::Number(n)) => Some(*n),
            _ => None,
        }
    }

    /// Extract static option pairs from Element/select's `options: LIST { ... }`.
    fn extract_select_options(&self, arguments: &[Spanned<Argument>]) -> Vec<(String, String)> {
        let mut options = Vec::new();
        let arg = match arguments.iter().find(|a| a.node.name.as_str() == "options") {
            Some(a) => a,
            None => return options,
        };
        let val = match arg.node.value.as_ref() {
            Some(v) => v,
            None => return options,
        };
        if let Expression::List { items } = &val.node {
            for item in items {
                if let Expression::Object(obj) = &item.node {
                    let mut value = String::new();
                    let mut label = String::new();
                    for field in &obj.variables {
                        let field_name = field.node.name.as_str();
                        if field_name == "value" {
                            if let Some(t) = Self::extract_static_text(&field.node.value.node) {
                                value = t;
                            }
                        } else if field_name == "label" {
                            if let Some(t) = Self::extract_static_text(&field.node.value.node) {
                                label = t;
                            }
                        }
                    }
                    if label.is_empty() {
                        label = value.clone();
                    }
                    options.push((value, label));
                }
            }
        }
        options
    }

    /// Try to extract a static text string from an expression.
    fn extract_static_text(expr: &Expression) -> Option<String> {
        match expr {
            Expression::TextLiteral { parts, .. } => {
                let mut result = String::new();
                for part in parts {
                    match part {
                        TextPart::Text(s) => result.push_str(s.as_str()),
                        _ => return None,
                    }
                }
                Some(result)
            }
            Expression::Literal(Literal::Text(s)) => Some(s.as_str().to_string()),
            _ => None,
        }
    }

    /// Find a named argument and ensure it's a cell.
    fn find_arg_cell(
        &mut self,
        arguments: &[Spanned<Argument>],
        name: &str,
        source: &Spanned<Expression>,
        span: Span,
    ) -> CellId {
        if let Some(arg) = arguments.iter().find(|a| a.node.name.as_str() == name) {
            if let Some(ref val) = arg.node.value {
                return self.lower_expr_to_cell(val, name);
            }
        }
        // Fallback: use the source expression as the argument.
        self.lower_expr_to_cell(source, name)
    }

    /// Like `lower_pipe` but takes an already-lowered CellId as the source.
    fn lower_pipe_with_source_cell(
        &mut self,
        target: CellId,
        source_cell: CellId,
        to: &Spanned<Expression>,
        var_span: Span,
    ) {
        match &to.node {
            Expression::Then { body } => {
                // Try to resolve event from the source cell (e.g., Timer output).
                let Some(trigger) = self.resolve_event_from_cell(source_cell) else {
                    if self.try_lower_then_from_latest_source(target, source_cell, body, body.span)
                    {
                        return;
                    }
                    let source_name = self
                        .cells
                        .get(source_cell.0 as usize)
                        .map(|cell| cell.name.as_str())
                        .unwrap_or("<unknown>");
                    self.errors.push(CompileError {
                        span: to.span,
                        message: format!(
                            "expected a concrete event source for THEN, but `{source_name}` does not resolve to an event ({})",
                            self.debug_source_cell_event_resolution(source_cell)
                        ),
                    });
                    self.nodes.push(IrNode::Derived {
                        cell: target,
                        expr: IrExpr::Constant(IrValue::Void),
                    });
                    return;
                };
                let body_expr = self.lower_expr(&body.node, body.span);
                self.propagate_then_result_metadata(target, &body_expr, body.span);
                self.nodes.push(IrNode::Then {
                    cell: target,
                    trigger,
                    body: body_expr,
                });
                self.cell_events.insert(target, trigger);
            }

            Expression::Hold { state_param, body } => {
                let init_expr = IrExpr::CellRead(source_cell);
                // Bind state parameter name to the HOLD cell so body can reference it.
                let state_name = state_param.as_str().to_string();
                let saved = self.name_to_cell.get(&state_name).copied();
                self.name_to_cell.insert(state_name.clone(), target);
                let trigger_bodies = self.lower_hold_body(&body.node, body.span, to.span);
                if let Some(prev) = saved {
                    self.name_to_cell.insert(state_name, prev);
                } else {
                    self.name_to_cell.remove(&state_name);
                }
                self.propagate_expr_field_cells(target, &init_expr);
                for (_trigger, body) in &trigger_bodies {
                    self.propagate_expr_field_cells(target, body);
                }
                self.nodes.push(IrNode::Hold {
                    cell: target,
                    init: init_expr,
                    trigger_bodies,
                });
            }

            Expression::When { arms } => {
                // Constant folding: if source is a known constant tag/value,
                // only lower the matching arm (skip all others).
                if self.resolve_event_from_cell(source_cell).is_none()
                    && let Some(const_val) = self.constant_cells.get(&source_cell).cloned()
                {
                    let mut folded = false;
                    for arm in arms {
                        if self.pattern_matches_constant(&arm.pattern, &const_val) {
                            // Bind destructured fields from TaggedObject patterns.
                            // E.g., for `ButtonIcon[checked] => body`, bind `checked`
                            // to the source cell's "checked" field cell.
                            let saved_bindings =
                                self.bind_tagged_pattern_fields(&arm.pattern, source_cell);

                            let body = self.lower_expr(&arm.body.node, arm.body.span);

                            // Restore bindings.
                            self.unbind_pattern_fields(saved_bindings);

                            // Propagate cell_field_cells through the Derived node
                            // so the bridge can resolve object fields downstream.
                            if let IrExpr::CellRead(src) = &body {
                                if let Some(fields) = self.resolve_cell_field_cells(*src) {
                                    self.cell_field_cells.insert(target, fields);
                                }
                            }
                            self.nodes.push(IrNode::Derived {
                                cell: target,
                                expr: body,
                            });
                            folded = true;
                            break;
                        }
                    }
                    if !folded {
                        // No arm matched the constant — emit void.
                        self.nodes.push(IrNode::Derived {
                            cell: target,
                            expr: IrExpr::Constant(IrValue::Void),
                        });
                    }
                } else {
                    let ir_arms: Vec<(IrPattern, IrExpr)> = arms
                        .iter()
                        .map(|arm| {
                            let pattern = self.lower_pattern(&arm.pattern);
                            // Bind pattern variable to source cell so body can reference it.
                            let saved = if let IrPattern::Binding(ref name) = pattern {
                                let prev = self.name_to_cell.get(name).copied();
                                self.name_to_cell.insert(name.clone(), source_cell);
                                Some((name.clone(), prev))
                            } else {
                                None
                            };
                            let body = self.lower_expr(&arm.body.node, arm.body.span);
                            // Restore previous binding.
                            if let Some((name, prev)) = saved {
                                if let Some(prev_cell) = prev {
                                    self.name_to_cell.insert(name, prev_cell);
                                } else {
                                    self.name_to_cell.remove(&name);
                                }
                            }
                            (pattern, body)
                        })
                        .collect();
                    // --- WHEN object distribution ---
                    // If all non-SKIP arms produce objects (ObjectConstruct or CellRead
                    // of object-store cells), distribute the WHEN across each field.
                    // This creates per-field WHEN cells that the bridge can process.
                    if self.try_distribute_when_object(target, source_cell, &ir_arms) {
                        self.nodes.push(IrNode::When {
                            cell: target,
                            source: source_cell,
                            arms: ir_arms,
                        });
                        // Distribution succeeded — target is now an object-store parent.
                    } else {
                        self.nodes.push(IrNode::When {
                            cell: target,
                            source: source_cell,
                            arms: ir_arms,
                        });
                    }
                }
            }

            Expression::While { arms } => {
                if self.resolve_event_from_cell(source_cell).is_none()
                    && let Some(const_val) = self.constant_cells.get(&source_cell).cloned()
                {
                    let mut folded = false;
                    for arm in arms {
                        if self.pattern_matches_constant(&arm.pattern, &const_val) {
                            let saved_bindings =
                                self.bind_tagged_pattern_fields(&arm.pattern, source_cell);
                            let body = self.lower_expr(&arm.body.node, arm.body.span);
                            self.unbind_pattern_fields(saved_bindings);
                            if let IrExpr::CellRead(src) = &body
                                && let Some(fields) = self.resolve_cell_field_cells(*src)
                            {
                                self.cell_field_cells.insert(target, fields);
                            }
                            self.nodes.push(IrNode::Derived {
                                cell: target,
                                expr: body,
                            });
                            folded = true;
                            break;
                        }
                    }
                    if !folded {
                        self.nodes.push(IrNode::Derived {
                            cell: target,
                            expr: IrExpr::Constant(IrValue::Void),
                        });
                    }
                    return;
                }

                let ir_arms: Vec<(IrPattern, IrExpr)> = arms
                    .iter()
                    .map(|arm| {
                        let pattern = self.lower_pattern(&arm.pattern);
                        // Bind pattern variable to source cell so body can reference it.
                        let saved = if let IrPattern::Binding(ref name) = pattern {
                            let prev = self.name_to_cell.get(name).copied();
                            self.name_to_cell.insert(name.clone(), source_cell);
                            Some((name.clone(), prev))
                        } else {
                            None
                        };
                        let body = self.lower_expr(&arm.body.node, arm.body.span);
                        // Restore previous binding.
                        if let Some((name, prev)) = saved {
                            if let Some(prev_cell) = prev {
                                self.name_to_cell.insert(name, prev_cell);
                            } else {
                                self.name_to_cell.remove(&name);
                            }
                        }
                        (pattern, body)
                    })
                    .collect();
                // Extract cell dependencies from arm bodies.
                let mut deps = Vec::new();
                for (_, body) in &ir_arms {
                    collect_cell_refs(body, &mut deps);
                }
                // Remove duplicates and exclude source cell.
                deps.sort_by_key(|c| c.0);
                deps.dedup();
                deps.retain(|c| *c != source_cell);
                if self.try_distribute_when_object(target, source_cell, &ir_arms) {
                    self.nodes.push(IrNode::While {
                        cell: target,
                        source: source_cell,
                        deps,
                        arms: ir_arms,
                    });
                    return;
                }
                self.nodes.push(IrNode::While {
                    cell: target,
                    source: source_cell,
                    deps,
                    arms: ir_arms,
                });
            }

            Expression::FunctionCall { path, arguments } => {
                let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                match path_strs.as_slice() {
                    ["Math", "sum"] => {
                        self.nodes.push(IrNode::MathSum {
                            cell: target,
                            input: source_cell,
                        });
                        // Propagate event source through MathSum.
                        if let Some(event) = self.cell_events.get(&source_cell).copied() {
                            self.cell_events.insert(target, event);
                        }
                    }
                    ["Document", "new"] => {
                        // Use explicit root arg if present, otherwise pipe source.
                        let root = arguments
                            .iter()
                            .find(|a| a.node.name.as_str() == "root")
                            .and_then(|a| a.node.value.as_ref())
                            .map(|v| self.lower_expr_to_cell(v, "doc_root"))
                            .unwrap_or(source_cell);
                        self.set_render_root(RenderSurface::Document, root, None, None);
                        self.nodes.push(IrNode::PipeThrough {
                            cell: target,
                            source: root,
                        });
                    }
                    ["Timer", "interval"] => {
                        // In pipe context with source_cell, use source_cell's value as duration.
                        let duration = IrExpr::CellRead(source_cell);
                        let event = self.alloc_event("timer", EventSource::Timer, to.span);
                        self.nodes.push(IrNode::Timer {
                            event,
                            interval_ms: duration,
                        });
                        self.nodes.push(IrNode::Derived {
                            cell: target,
                            expr: IrExpr::Constant(IrValue::Void),
                        });
                        self.cell_events.insert(target, event);
                    }
                    ["Stream", "pulses"] => {
                        self.nodes.push(IrNode::PipeThrough {
                            cell: target,
                            source: source_cell,
                        });
                        // Propagate field cell info through pass-through.
                        if let Some(fields) = self.cell_field_cells.get(&source_cell).cloned() {
                            self.cell_field_cells.insert(target, fields);
                        }
                    }
                    ["Stream", "skip"] => {
                        let skip_count = arguments
                            .iter()
                            .find(|arg| arg.node.name.as_str() == "count")
                            .and_then(|arg| arg.node.value.as_ref())
                            .and_then(|value| match &value.node {
                                Expression::Literal(
                                    crate::parser::static_expression::Literal::Number(n),
                                ) if *n >= 0.0 => Some(*n as usize),
                                _ => None,
                            })
                            .unwrap_or(0);
                        let seen_cell = self.alloc_cell("stream_skip_seen", to.span);
                        self.nodes.push(IrNode::Derived {
                            cell: seen_cell,
                            expr: IrExpr::Constant(IrValue::Number(0.0)),
                        });
                        self.nodes.push(IrNode::StreamSkip {
                            cell: target,
                            source: source_cell,
                            count: skip_count,
                            seen_cell,
                        });
                        if let Some(fields) = self.cell_field_cells.get(&source_cell).cloned() {
                            self.cell_field_cells.insert(target, fields);
                        }
                    }
                    ["List", "get"] => {
                        if let Some(index) = self.find_constant_index_argument(arguments, to.span)
                            && let Some(item_expr) = self.try_resolve_list_get_from_cell(
                                source_cell,
                                index,
                                &mut HashSet::new(),
                            )
                        {
                            self.propagate_expr_field_cells(target, &item_expr);
                            self.nodes.push(IrNode::Derived {
                                cell: target,
                                expr: item_expr,
                            });
                            return;
                        }
                        self.nodes.push(IrNode::PipeThrough {
                            cell: target,
                            source: source_cell,
                        });
                        let metadata_source = self
                            .find_metadata_source_cell(source_cell)
                            .unwrap_or(source_cell);
                        let shape_source = if self.find_list_constructor(source_cell).is_some()
                            || self.find_list_item_field_exprs(source_cell).is_some()
                        {
                            source_cell
                        } else {
                            metadata_source
                        };
                        if std::env::var("BOON_WASM_LIST_GET_TRACE").is_ok() {
                            eprintln!(
                                "[list_get_stmt_fallback] target={} source={} metadata={} shape={} ctor(source)={:?} ctor(shape)={:?} fields(source)={} item_fields(source)={} fields(shape)={} item_fields(shape)={}",
                                self.cells[target.0 as usize].name,
                                self.cells[source_cell.0 as usize].name,
                                self.cells[metadata_source.0 as usize].name,
                                self.cells[shape_source.0 as usize].name,
                                self.find_list_constructor(source_cell),
                                self.find_list_constructor(shape_source),
                                self.cell_field_cells
                                    .get(&source_cell)
                                    .map(|m| m.len())
                                    .unwrap_or(0),
                                self.find_list_item_field_exprs(source_cell)
                                    .map(|m| m.len())
                                    .unwrap_or(0),
                                self.cell_field_cells
                                    .get(&shape_source)
                                    .map(|m| m.len())
                                    .unwrap_or(0),
                                self.find_list_item_field_exprs(shape_source)
                                    .map(|m| m.len())
                                    .unwrap_or(0),
                            );
                        }
                        let _ =
                            self.materialize_list_item_field_cells(target, shape_source, to.span);
                        self.propagate_list_constructor(shape_source, target);
                    }
                    ["List", "append"] => {
                        let item_arg = arguments.iter().find(|a| a.node.name.as_str() == "item");
                        if let Some(arg) = item_arg {
                            if let Some(ref val) = arg.node.value {
                                // Extract constructor from item argument.
                                let item_constructor =
                                    self.extract_constructor_from_expr(&val.node);
                                let reactive_dep = self.find_reactive_cell_in_expr(&val.node);
                                let item_cell = self.lower_expr_to_cell(val, "append_item");
                                let on_arg =
                                    arguments.iter().find(|a| a.node.name.as_str() == "on");
                                let trigger = on_arg
                                    .and_then(|a| a.node.value.as_ref())
                                    .and_then(|v| self.resolve_event_from_expr(&v.node))
                                    .or_else(|| self.resolve_event_from_cell(item_cell))
                                    .or_else(|| self.resolve_event_from_expr(&val.node))
                                    .unwrap_or_else(|| {
                                        self.alloc_event(
                                            "list_append_trigger",
                                            EventSource::Synthetic,
                                            to.span,
                                        )
                                    });
                                let watch_cell = if matches!(
                                    self.events[trigger.0 as usize].source,
                                    EventSource::Synthetic
                                ) {
                                    reactive_dep
                                } else {
                                    None
                                };
                                self.nodes.push(IrNode::ListAppend {
                                    cell: target,
                                    source: source_cell,
                                    item: item_cell,
                                    trigger,
                                    watch_cell,
                                });
                                // Register constructor from item arg (takes priority),
                                // then propagate from source as fallback.
                                if let Some(ctor) = item_constructor {
                                    self.list_item_constructor.insert(target, ctor);
                                } else {
                                    self.propagate_list_constructor(source_cell, target);
                                }
                                return;
                            }
                        }
                        self.nodes.push(IrNode::PipeThrough {
                            cell: target,
                            source: source_cell,
                        });
                        self.propagate_list_constructor(source_cell, target);
                    }
                    ["List", "clear"] => {
                        let on_arg = arguments.iter().find(|a| a.node.name.as_str() == "on");
                        if let Some(arg) = on_arg {
                            if let Some(ref val) = arg.node.value {
                                let trigger = self
                                    .resolve_event_from_expr(&val.node)
                                    .or_else(|| {
                                        let trigger_cell =
                                            self.lower_expr_to_cell(val, "clear_trigger");
                                        self.resolve_event_from_cell(trigger_cell)
                                    })
                                    .unwrap_or_else(|| {
                                        self.alloc_event(
                                            "list_clear_trigger",
                                            EventSource::Synthetic,
                                            to.span,
                                        )
                                    });
                                self.nodes.push(IrNode::ListClear {
                                    cell: target,
                                    source: source_cell,
                                    trigger,
                                });
                                self.propagate_list_constructor(source_cell, target);
                                return;
                            }
                        }
                        self.nodes.push(IrNode::PipeThrough {
                            cell: target,
                            source: source_cell,
                        });
                        self.propagate_list_constructor(source_cell, target);
                    }
                    ["List", "count"] => {
                        self.nodes.push(IrNode::ListCount {
                            cell: target,
                            source: source_cell,
                        });
                    }
                    ["List", "map"] => {
                        let item_name = arguments
                            .first()
                            .map(|a| a.node.name.as_str().to_string())
                            .unwrap_or_else(|| "item".to_string());
                        let template_arg = arguments
                            .iter()
                            .find(|a| matches!(a.node.name.as_str(), "to" | "new"));
                        if let Some(arg) = template_arg {
                            if let Some(ref val) = arg.node.value {
                                let template_constructor =
                                    self.extract_constructor_from_expr(&val.node);
                                // Record range start before template lowering.
                                let cell_start = self.cells.len() as u32;
                                let event_start = self.events.len() as u32;
                                let item_cell = self.alloc_cell(&item_name, to.span);
                                self.name_to_cell.insert(item_name.clone(), item_cell);
                                // Inline the list item constructor for per-item cells.
                                self.inline_list_constructor_for_template(
                                    source_cell,
                                    item_cell,
                                    &item_name,
                                    to.span,
                                );
                                let _ = self.materialize_list_item_field_cells(
                                    item_cell,
                                    source_cell,
                                    to.span,
                                );
                                let template_expr = self.lower_expr(&val.node, val.span);
                                let template_item_fields = self
                                    .extract_item_field_exprs_from_template_expr(&template_expr);
                                let template_cell = self.alloc_cell("list_map_template", to.span);
                                let template_node = IrNode::Derived {
                                    cell: template_cell,
                                    expr: template_expr,
                                };
                                // Resolve deferred per-item removes (same as primary List/map handler).
                                if let Some(removes) =
                                    self.pending_per_item_removes.remove(&source_cell)
                                {
                                    for remove in removes {
                                        let saved =
                                            self.name_to_cell.get(&remove.item_name).copied();
                                        let bind_cell = self
                                            .name_to_cell
                                            .get(&item_name)
                                            .copied()
                                            .unwrap_or(item_cell);
                                        self.name_to_cell
                                            .insert(remove.item_name.clone(), bind_cell);
                                        if let Some(event_id) =
                                            self.resolve_event_from_expr(&remove.on_expr.node)
                                        {
                                            if let IrNode::ListRemove { trigger, .. } =
                                                &mut self.nodes[remove.node_index]
                                            {
                                                *trigger = event_id;
                                            }
                                        }
                                        if let Some(s) = saved {
                                            self.name_to_cell.insert(remove.item_name.clone(), s);
                                        } else {
                                            self.name_to_cell.remove(&remove.item_name);
                                        }
                                    }
                                }
                                let cell_end = self.cells.len() as u32;
                                let event_end = self.events.len() as u32;
                                self.nodes.push(IrNode::ListMap {
                                    cell: target,
                                    source: source_cell,
                                    item_name,
                                    item_cell,
                                    template: Box::new(template_node),
                                    template_cell_range: (cell_start, cell_end),
                                    template_event_range: (event_start, event_end),
                                });
                                if let Some(constructor) = template_constructor {
                                    self.list_item_constructor.insert(target, constructor);
                                }
                                if let Some(fields) = template_item_fields {
                                    self.list_item_field_exprs.insert(target, fields);
                                }
                                return;
                            }
                        }
                        self.nodes.push(IrNode::PipeThrough {
                            cell: target,
                            source: source_cell,
                        });
                    }
                    ["List", "latest"] => {
                        if let Some(arms) = self.lower_list_latest_from_source(source_cell) {
                            for arm in &arms {
                                self.propagate_expr_field_cells(target, &arm.body);
                            }
                            self.nodes.push(IrNode::Latest { target, arms });
                        } else {
                            self.nodes.push(IrNode::PipeThrough {
                                cell: target,
                                source: source_cell,
                            });
                        }
                    }
                    ["List", "is_empty"] => {
                        self.nodes.push(IrNode::ListIsEmpty {
                            cell: target,
                            source: source_cell,
                        });
                    }
                    ["List", "is_not_empty"] => {
                        let is_empty_cell = self.alloc_cell("list_is_empty_intermediate", var_span);
                        self.nodes.push(IrNode::ListIsEmpty {
                            cell: is_empty_cell,
                            source: source_cell,
                        });
                        self.nodes.push(IrNode::Derived {
                            cell: target,
                            expr: IrExpr::Not(Box::new(IrExpr::CellRead(is_empty_cell))),
                        });
                    }
                    ["List", "remove"] => {
                        // Bind the item parameter so the `on:` expression can reference item fields.
                        let item_name = arguments
                            .first()
                            .filter(|a| {
                                let n = a.node.name.as_str();
                                n != "on" && n != "if"
                            })
                            .map(|a| a.node.name.as_str().to_string())
                            .unwrap_or_else(|| "item".to_string());
                        let saved_item = self.name_to_cell.get(&item_name).copied();
                        let item_cell = self.alloc_cell(&item_name, to.span);
                        self.name_to_cell.insert(item_name.clone(), item_cell);
                        self.nodes.push(IrNode::Derived {
                            cell: item_cell,
                            expr: IrExpr::Constant(IrValue::Void),
                        });

                        let on_arg = arguments.iter().find(|a| a.node.name.as_str() == "on");
                        if let Some(arg) = on_arg {
                            if let Some(ref val) = arg.node.value {
                                // Case 2: Pipe { from: <event>, to: Then { body } } with per-item predicate.
                                if let Expression::Pipe {
                                    from,
                                    to: then_expr,
                                } = &val.node
                                {
                                    if let Expression::Then { body } = &then_expr.node {
                                        if let Some(event_id) =
                                            self.resolve_event_from_expr(&from.node).or_else(|| {
                                                let c =
                                                    self.lower_expr_to_cell(from, "remove_event");
                                                self.resolve_event_from_cell(c)
                                            })
                                        {
                                            // Pre-scan THEN body for item field references.
                                            let field_names =
                                                collect_item_field_names(body, &item_name);
                                            let mut item_field_cells = Vec::new();
                                            if !field_names.is_empty() {
                                                let mut field_map = HashMap::new();
                                                for field in &field_names {
                                                    let sub_cell = self.alloc_cell(
                                                        &format!("{}.{}", item_name, field),
                                                        to.span,
                                                    );
                                                    self.nodes.push(IrNode::Derived {
                                                        cell: sub_cell,
                                                        expr: IrExpr::Constant(IrValue::Void),
                                                    });
                                                    self.name_to_cell.insert(
                                                        format!("{}.{}", item_name, field),
                                                        sub_cell,
                                                    );
                                                    field_map.insert(field.clone(), sub_cell);
                                                    item_field_cells
                                                        .push((field.clone(), sub_cell));
                                                }
                                                if !field_map.is_empty() {
                                                    self.cell_field_cells
                                                        .insert(item_cell, field_map);
                                                }
                                            }
                                            // Lower THEN body to get predicate cell.
                                            let predicate_cell = if !item_field_cells.is_empty() {
                                                Some(self.lower_expr_to_cell(body, "remove_pred"))
                                            } else {
                                                None
                                            };
                                            // Restore item binding.
                                            if let Some(prev) = saved_item {
                                                self.name_to_cell.insert(item_name.clone(), prev);
                                            } else {
                                                self.name_to_cell.remove(&item_name);
                                            }
                                            // Clean up sub-cell name_to_cell entries.
                                            for (field, _) in &item_field_cells {
                                                self.name_to_cell
                                                    .remove(&format!("{}.{}", item_name, field));
                                            }
                                            self.nodes.push(IrNode::ListRemove {
                                                cell: target,
                                                source: source_cell,
                                                trigger: event_id,
                                                predicate: predicate_cell,
                                                item_cell: if item_field_cells.is_empty() {
                                                    None
                                                } else {
                                                    Some(item_cell)
                                                },
                                                item_field_cells,
                                            });
                                            self.propagate_list_constructor(source_cell, target);
                                            return;
                                        }
                                    }
                                }
                                // Case 1: Direct event (per-item or simple global).
                                let trigger = self.resolve_event_from_expr(&val.node);
                                if let Some(trigger) = trigger {
                                    if let Some(prev) = saved_item {
                                        self.name_to_cell.insert(item_name, prev);
                                    } else {
                                        self.name_to_cell.remove(&item_name);
                                    }
                                    self.nodes.push(IrNode::ListRemove {
                                        cell: target,
                                        source: source_cell,
                                        trigger,
                                        predicate: None,
                                        item_cell: None,
                                        item_field_cells: vec![],
                                    });
                                    self.propagate_list_constructor(source_cell, target);
                                    return;
                                }
                                // Check if per-item event, defer to List/map.
                                let is_per_item_event =
                                    Self::is_per_item_event_expr(&val.node, &item_name);
                                if is_per_item_event {
                                    if let Some(prev) = saved_item {
                                        self.name_to_cell.insert(item_name.clone(), prev);
                                    } else {
                                        self.name_to_cell.remove(&item_name);
                                    }
                                    let placeholder = EventId(u32::MAX);
                                    let node_index = self.nodes.len();
                                    self.nodes.push(IrNode::ListRemove {
                                        cell: target,
                                        source: source_cell,
                                        trigger: placeholder,
                                        predicate: None,
                                        item_cell: None,
                                        item_field_cells: vec![],
                                    });
                                    self.pending_per_item_removes
                                        .entry(source_cell)
                                        .or_default()
                                        .push(PendingPerItemRemove {
                                            item_name,
                                            node_index,
                                            on_expr: val.clone(),
                                        });
                                    self.propagate_list_constructor(source_cell, target);
                                    return;
                                }
                                // Fallback: lower expression to cell.
                                let trigger = {
                                    let trigger_cell =
                                        self.lower_expr_to_cell(val, "remove_trigger");
                                    self.resolve_event_from_cell(trigger_cell)
                                }
                                .unwrap_or_else(|| {
                                    self.alloc_event(
                                        "list_remove_trigger",
                                        EventSource::Synthetic,
                                        to.span,
                                    )
                                });
                                if let Some(prev) = saved_item {
                                    self.name_to_cell.insert(item_name, prev);
                                } else {
                                    self.name_to_cell.remove(&item_name);
                                }
                                self.nodes.push(IrNode::ListRemove {
                                    cell: target,
                                    source: source_cell,
                                    trigger,
                                    predicate: None,
                                    item_cell: None,
                                    item_field_cells: vec![],
                                });
                                self.propagate_list_constructor(source_cell, target);
                                return;
                            }
                        }
                        if let Some(prev) = saved_item {
                            self.name_to_cell.insert(item_name, prev);
                        } else {
                            self.name_to_cell.remove(&item_name);
                        }
                        self.nodes.push(IrNode::PipeThrough {
                            cell: target,
                            source: source_cell,
                        });
                        self.propagate_list_constructor(source_cell, target);
                    }
                    ["List", "retain"] => {
                        // Bind the item parameter so the `if:` expression can reference item fields.
                        let item_name = arguments
                            .first()
                            .filter(|a| {
                                let n = a.node.name.as_str();
                                n != "on" && n != "if"
                            })
                            .map(|a| a.node.name.as_str().to_string())
                            .unwrap_or_else(|| "item".to_string());
                        let saved_item = self.name_to_cell.get(&item_name).copied();
                        let item_cell = self.alloc_cell(&item_name, to.span);
                        self.name_to_cell.insert(item_name.clone(), item_cell);
                        self.nodes.push(IrNode::Derived {
                            cell: item_cell,
                            expr: IrExpr::Constant(IrValue::Void),
                        });
                        // Pre-scan the `if:` expression for field accesses on item.
                        let if_expr = arguments
                            .iter()
                            .find(|a| a.node.name.as_str() == "if")
                            .and_then(|a| a.node.value.as_ref());
                        let mut item_field_cells = Vec::new();
                        if let Some(val) = if_expr {
                            let field_names = collect_item_field_names(val, &item_name);
                            let mut field_map = HashMap::new();
                            for field in &field_names {
                                let sub_cell =
                                    self.alloc_cell(&format!("{}.{}", item_name, field), to.span);
                                self.nodes.push(IrNode::Derived {
                                    cell: sub_cell,
                                    expr: IrExpr::Constant(IrValue::Void),
                                });
                                self.name_to_cell
                                    .insert(format!("{}.{}", item_name, field), sub_cell);
                                field_map.insert(field.clone(), sub_cell);
                                item_field_cells.push((field.clone(), sub_cell));
                            }
                            if !field_map.is_empty() {
                                self.cell_field_cells.insert(item_cell, field_map);
                            }
                        }
                        // Lower the `if:` predicate expression and capture it as a cell.
                        let predicate_cell =
                            if_expr.map(|val| self.lower_expr_to_cell(val, "retain_pred"));
                        // Restore item binding.
                        if let Some(prev) = saved_item {
                            self.name_to_cell.insert(item_name.clone(), prev);
                        } else {
                            self.name_to_cell.remove(&item_name);
                        }
                        // Clean up sub-cell name_to_cell entries.
                        for (field, _) in &item_field_cells {
                            self.name_to_cell
                                .remove(&format!("{}.{}", item_name, field));
                        }
                        self.nodes.push(IrNode::ListRetain {
                            cell: target,
                            source: source_cell,
                            predicate: predicate_cell,
                            item_cell: if item_field_cells.is_empty() {
                                None
                            } else {
                                Some(item_cell)
                            },
                            item_field_cells,
                        });
                        self.propagate_list_constructor(source_cell, target);
                    }
                    ["List", "every"] | ["List", "any"] => {
                        let is_every = path_strs[1] == "every";
                        // Bind the item parameter (same pattern as List/retain).
                        let item_name = arguments
                            .first()
                            .filter(|a| {
                                let n = a.node.name.as_str();
                                n != "on" && n != "if"
                            })
                            .map(|a| a.node.name.as_str().to_string())
                            .unwrap_or_else(|| "item".to_string());
                        let saved_item = self.name_to_cell.get(&item_name).copied();
                        let item_cell = self.alloc_cell(&item_name, to.span);
                        self.name_to_cell.insert(item_name.clone(), item_cell);
                        self.nodes.push(IrNode::Derived {
                            cell: item_cell,
                            expr: IrExpr::Constant(IrValue::Void),
                        });
                        let if_expr = arguments
                            .iter()
                            .find(|a| a.node.name.as_str() == "if")
                            .and_then(|a| a.node.value.as_ref());
                        let mut item_field_cells = Vec::new();
                        if let Some(val) = if_expr {
                            let field_names = collect_item_field_names(val, &item_name);
                            let mut field_map = HashMap::new();
                            for field in &field_names {
                                let sub_cell =
                                    self.alloc_cell(&format!("{}.{}", item_name, field), to.span);
                                self.nodes.push(IrNode::Derived {
                                    cell: sub_cell,
                                    expr: IrExpr::Constant(IrValue::Void),
                                });
                                self.name_to_cell
                                    .insert(format!("{}.{}", item_name, field), sub_cell);
                                field_map.insert(field.clone(), sub_cell);
                                item_field_cells.push((field.clone(), sub_cell));
                            }
                            if !field_map.is_empty() {
                                self.cell_field_cells.insert(item_cell, field_map);
                            }
                        }
                        let predicate_cell =
                            if_expr.map(|val| self.lower_expr_to_cell(val, "check_pred"));
                        if let Some(prev) = saved_item {
                            self.name_to_cell.insert(item_name.clone(), prev);
                        } else {
                            self.name_to_cell.remove(&item_name);
                        }
                        for (field, _) in &item_field_cells {
                            self.name_to_cell
                                .remove(&format!("{}.{}", item_name, field));
                        }
                        if is_every {
                            self.nodes.push(IrNode::ListEvery {
                                cell: target,
                                source: source_cell,
                                predicate: predicate_cell,
                                item_cell: Some(item_cell),
                                item_field_cells,
                            });
                        } else {
                            self.nodes.push(IrNode::ListAny {
                                cell: target,
                                source: source_cell,
                                predicate: predicate_cell,
                                item_cell: Some(item_cell),
                                item_field_cells,
                            });
                        }
                    }
                    ["Text", "trim"] => {
                        self.nodes.push(IrNode::TextTrim {
                            cell: target,
                            source: source_cell,
                        });
                    }
                    ["Text", "is_empty"] => {
                        self.nodes.push(IrNode::CustomCall {
                            cell: target,
                            path: vec!["Text".to_string(), "is_empty".to_string()],
                            args: vec![("__source".to_string(), IrExpr::CellRead(source_cell))],
                        });
                    }
                    ["Text", "is_not_empty"] => {
                        self.nodes.push(IrNode::TextIsNotEmpty {
                            cell: target,
                            source: source_cell,
                        });
                    }
                    ["Text", "starts_with"] => {
                        let prefix_cell = if let Some(prefix_arg) =
                            arguments.iter().find(|a| a.node.name.as_str() == "prefix")
                        {
                            if let Some(ref val) = prefix_arg.node.value {
                                self.lower_expr_to_cell(val, "starts_with_prefix")
                            } else {
                                self.alloc_cell("starts_with_prefix_empty", var_span)
                            }
                        } else {
                            self.alloc_cell("starts_with_prefix_empty", var_span)
                        };
                        self.nodes.push(IrNode::TextStartsWith {
                            cell: target,
                            source: source_cell,
                            prefix: prefix_cell,
                        });
                    }
                    ["Text", "length"] => {
                        self.nodes.push(IrNode::CustomCall {
                            cell: target,
                            path: vec!["Text".to_string(), "length".to_string()],
                            args: vec![("__source".to_string(), IrExpr::CellRead(source_cell))],
                        });
                    }
                    ["Text", "find"] => {
                        let search_cell = arguments
                            .iter()
                            .find(|a| a.node.name.as_str() == "search")
                            .and_then(|a| a.node.value.as_ref())
                            .map(|v| self.lower_expr_to_cell(v, "text_find_search"))
                            .unwrap_or_else(|| self.alloc_cell("text_find_search_empty", var_span));
                        self.nodes.push(IrNode::CustomCall {
                            cell: target,
                            path: vec!["Text".to_string(), "find".to_string()],
                            args: vec![
                                ("__source".to_string(), IrExpr::CellRead(source_cell)),
                                ("search".to_string(), IrExpr::CellRead(search_cell)),
                            ],
                        });
                    }
                    ["Text", "substring"] => {
                        let start_cell = arguments
                            .iter()
                            .find(|a| a.node.name.as_str() == "start")
                            .and_then(|a| a.node.value.as_ref())
                            .map(|v| self.lower_expr_to_cell(v, "text_substring_start"))
                            .unwrap_or_else(|| {
                                self.alloc_cell("text_substring_start_empty", var_span)
                            });
                        let length_cell = arguments
                            .iter()
                            .find(|a| a.node.name.as_str() == "length")
                            .and_then(|a| a.node.value.as_ref())
                            .map(|v| self.lower_expr_to_cell(v, "text_substring_length"))
                            .unwrap_or_else(|| {
                                self.alloc_cell("text_substring_length_empty", var_span)
                            });
                        self.nodes.push(IrNode::CustomCall {
                            cell: target,
                            path: vec!["Text".to_string(), "substring".to_string()],
                            args: vec![
                                ("__source".to_string(), IrExpr::CellRead(source_cell)),
                                ("start".to_string(), IrExpr::CellRead(start_cell)),
                                ("length".to_string(), IrExpr::CellRead(length_cell)),
                            ],
                        });
                    }
                    ["Text", "to_number"] => {
                        let nan_tag_value = self.intern_tag("NaN");
                        self.nodes.push(IrNode::TextToNumber {
                            cell: target,
                            source: source_cell,
                            nan_tag_value,
                        });
                    }
                    ["Math", "round"] => {
                        self.nodes.push(IrNode::MathRound {
                            cell: target,
                            source: source_cell,
                        });
                    }
                    ["Math", "min"] => {
                        let b_cell = arguments
                            .iter()
                            .find(|a| a.node.name.as_str() == "b")
                            .and_then(|a| a.node.value.as_ref())
                            .map(|v| self.lower_expr_to_cell(v, "min_b"))
                            .unwrap_or_else(|| self.alloc_cell("min_b_empty", var_span));
                        self.nodes.push(IrNode::MathMin {
                            cell: target,
                            source: source_cell,
                            b: b_cell,
                        });
                    }
                    ["Math", "max"] => {
                        let b_cell = arguments
                            .iter()
                            .find(|a| a.node.name.as_str() == "b")
                            .and_then(|a| a.node.value.as_ref())
                            .map(|v| self.lower_expr_to_cell(v, "max_b"))
                            .unwrap_or_else(|| self.alloc_cell("max_b_empty", var_span));
                        self.nodes.push(IrNode::MathMax {
                            cell: target,
                            source: source_cell,
                            b: b_cell,
                        });
                    }
                    ["Bool", "not"] => {
                        self.nodes.push(IrNode::Derived {
                            cell: target,
                            expr: IrExpr::Not(Box::new(IrExpr::CellRead(source_cell))),
                        });
                    }
                    // --- `source |> Bool/toggle(when: event)` ---
                    ["Bool", "toggle"] => {
                        let when_arg = arguments.iter().find(|a| a.node.name.as_str() == "when");
                        if let Some(when_arg) = when_arg {
                            if let Some(ref val) = when_arg.node.value {
                                // Try event resolution from expression FIRST (handles
                                // element.event.click paths), then fall back to cell-based.
                                let trigger =
                                    self.resolve_event_from_expr(&val.node).unwrap_or_else(|| {
                                        let when_cell = self.lower_expr_to_cell(val, "toggle_when");
                                        self.resolve_event_from_cell(when_cell).unwrap_or_else(
                                            || {
                                                self.alloc_event(
                                                    "toggle_trigger",
                                                    EventSource::Synthetic,
                                                    to.span,
                                                )
                                            },
                                        )
                                    });
                                self.nodes.push(IrNode::Hold {
                                    cell: target,
                                    init: IrExpr::CellRead(source_cell),
                                    trigger_bodies: vec![(
                                        trigger,
                                        IrExpr::Not(Box::new(IrExpr::CellRead(target))),
                                    )],
                                });
                            } else {
                                self.nodes.push(IrNode::PipeThrough {
                                    cell: target,
                                    source: source_cell,
                                });
                            }
                        } else {
                            self.nodes.push(IrNode::PipeThrough {
                                cell: target,
                                source: source_cell,
                            });
                        }
                    }
                    ["Router", "go_to"] => {
                        self.nodes.push(IrNode::RouterGoTo {
                            cell: target,
                            source: source_cell,
                        });
                    }
                    _ => {
                        // Check for user-defined function.
                        let resolved_fn = if path_strs.len() == 1 {
                            self.resolve_func_name(path_strs[0])
                        } else if path_strs.len() == 2 {
                            let qualified = format!("{}/{}", path_strs[0], path_strs[1]);
                            self.resolve_func_name(&qualified)
                        } else {
                            None
                        };
                        if let Some(fn_name) = resolved_fn {
                            let func_def = self.find_func_def(&fn_name).unwrap();
                            let func_id = self.name_to_func[&fn_name];
                            self.current_module = self.function_modules.get(&fn_name).cloned();
                            let result = self.inline_function_call_with_pipe(
                                &fn_name,
                                func_id,
                                &func_def,
                                source_cell,
                                arguments,
                                to.span,
                            );
                            match result {
                                IrExpr::CellRead(result_cell) => {
                                    self.repair_returned_cell_shape(result_cell, to.span);
                                    self.nodes.push(IrNode::PipeThrough {
                                        cell: target,
                                        source: result_cell,
                                    });
                                    // Propagate events from result cell.
                                    if let Some(event) = self.cell_events.get(&result_cell).copied()
                                    {
                                        self.cell_events.insert(target, event);
                                    }
                                    // Propagate cell_field_cells through PipeThrough.
                                    if let Some(fields) =
                                        self.cell_field_cells.get(&result_cell).cloned()
                                    {
                                        self.cell_field_cells.insert(target, fields);
                                    }
                                }
                                _ => {
                                    self.nodes.push(IrNode::Derived {
                                        cell: target,
                                        expr: result,
                                    });
                                }
                            }
                            return;
                        }
                        // Fallback: generic function call with source as pipe-through.
                        let source_span = crate::parser::span_at(0);
                        let source_expr = spanned(Expression::Skip, source_span);
                        self.lower_pipe_to_function_call(
                            target,
                            &source_expr,
                            path,
                            arguments,
                            to.span,
                            var_span,
                        );
                        // Propagate field cell info through pass-through function calls.
                        if let Some(fields) = self.cell_field_cells.get(&source_cell).cloned() {
                            self.cell_field_cells.insert(target, fields);
                        }
                    }
                }
            }

            Expression::FieldAccess { path } => {
                // Try to resolve through cell_field_cells (for object HOLD state).
                if path.len() == 1 {
                    let field = path[0].as_str();
                    // Trace through PipeThrough/CustomCall chain to find source with field cells.
                    let origin = self.trace_pipe_source(source_cell);
                    if let Some(field_cells) = self.cell_field_cells.get(&origin) {
                        if let Some(&field_cell) = field_cells.get(field) {
                            self.nodes.push(IrNode::Derived {
                                cell: target,
                                expr: IrExpr::CellRead(field_cell),
                            });
                            return;
                        }
                    }
                }
                let mut expr: IrExpr = IrExpr::CellRead(source_cell);
                for field in path {
                    expr = IrExpr::FieldAccess {
                        object: Box::new(expr),
                        field: field.as_str().to_string(),
                    };
                }
                self.nodes.push(IrNode::Derived { cell: target, expr });
            }

            Expression::LinkSetter { alias } => {
                // `element |> LINK { target }` — connect element's events to the
                // LINK target. The target resolves to a LINK placeholder cell whose
                // events were pre-allocated during object flattening.
                // Resolve the LINK target to get the element name for event association.
                let target_name = self.resolve_link_target_name(&alias.node, alias.span);
                if let Some(ref name) = target_name {
                    // Set current_var_name to the LINK target so the source element
                    // picks up the target's pre-allocated events.
                    let saved_var_name = self.current_var_name.take();
                    self.current_var_name = Some(name.clone());
                    // Re-lower the source element with the LINK target's events.
                    // The element has already been lowered to source_cell, but
                    // we need to create a pass-through with the right var_name.
                    self.nodes.push(IrNode::PipeThrough {
                        cell: target,
                        source: source_cell,
                    });
                    self.current_var_name = saved_var_name;
                } else {
                    self.nodes.push(IrNode::PipeThrough {
                        cell: target,
                        source: source_cell,
                    });
                }
            }

            Expression::Pipe {
                from: mid,
                to: final_to,
            } => {
                let mid_cell = self.alloc_cell("pipe_mid", to.span);
                self.lower_pipe_with_source_cell(mid_cell, source_cell, mid, var_span);
                self.lower_pipe_with_source_cell(target, mid_cell, final_to, var_span);
            }

            _ => {
                // Fallback: pipe through.
                self.nodes.push(IrNode::PipeThrough {
                    cell: target,
                    source: source_cell,
                });
            }
        }
    }

    fn propagate_then_result_metadata(
        &mut self,
        target: CellId,
        body_expr: &IrExpr,
        body_span: Span,
    ) {
        self.propagate_expr_field_cells(target, body_expr);
        match body_expr {
            IrExpr::CellRead(result_cell) => {
                if let Some(fields) = self.cell_field_cells.get(result_cell).cloned() {
                    self.cell_field_cells.insert(target, fields);
                }
                if let Some(constructor) = self.list_item_constructor.get(result_cell).cloned() {
                    self.list_item_constructor.insert(target, constructor);
                }
            }
            IrExpr::TaggedObject { fields, .. } | IrExpr::ObjectConstruct(fields) => {
                let field_map = self.register_inline_object_field_cells(target, fields, body_span);
                self.cell_field_cells.insert(target, field_map);
            }
            _ => {}
        }
    }

    fn try_lower_then_from_latest_source(
        &mut self,
        target: CellId,
        source_cell: CellId,
        body: &Spanned<Expression>,
        body_span: Span,
    ) -> bool {
        let Some(arms) = self.extract_latest_arms_from_cell(source_cell, (0, u32::MAX)) else {
            return false;
        };
        let trigger_arms: Vec<LatestArm> = arms
            .into_iter()
            .filter(|arm| arm.trigger.is_some())
            .collect();
        if trigger_arms.is_empty() {
            return false;
        }

        let body_expr = self.lower_expr(&body.node, body_span);
        self.propagate_then_result_metadata(target, &body_expr, body_span);
        self.nodes.push(IrNode::Latest {
            target,
            arms: trigger_arms
                .into_iter()
                .map(|arm| LatestArm {
                    trigger: arm.trigger,
                    body: body_expr.clone(),
                })
                .collect(),
        });
        true
    }

    /// Trace a cell through PipeThrough/CustomCall chains to find the original source.
    fn trace_pipe_source(&self, cell: CellId) -> CellId {
        for node in &self.nodes {
            match node {
                IrNode::PipeThrough { cell: c, source } if *c == cell => {
                    return self.trace_pipe_source(*source);
                }
                IrNode::CustomCall { cell: c, .. } if *c == cell => {
                    // CustomCall doesn't have a direct source, but the previous
                    // pipe chain would have set up the source. Just return as-is.
                    return cell;
                }
                _ => {}
            }
        }
        cell
    }

    /// Extract LINK event bindings for an element.
    /// Looks up the pre-allocated EventIds from Pass 1.5 scanning.
    fn extract_links_for_element(&self, element_var_name: &str) -> Vec<(String, EventId)> {
        if let Some(ref outer_name) = self.current_var_name {
            // While lowering through `... |> LINK { target }`, always prefer the
            // target's pre-allocated events so the element and its consumers
            // share the same EventIds.
            if let Some(events) = self.element_events.get(outer_name) {
                return events
                    .iter()
                    .map(|(name, &eid)| (name.clone(), eid))
                    .collect();
            }
        }

        self.element_events
            .get(element_var_name)
            .map(|events| {
                events
                    .iter()
                    .map(|(name, &eid)| (name.clone(), eid))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn rebind_element_links_to_target(&mut self, cell: CellId, target_name: &str) {
        let Some(target_events) = self.element_events.get(target_name).cloned() else {
            return;
        };

        let mut text_source_cell = None;
        for node in self.nodes.iter_mut().rev() {
            match node {
                IrNode::Element {
                    cell: node_cell,
                    links,
                    kind,
                    ..
                } if *node_cell == cell => {
                    *links = target_events
                        .iter()
                        .map(|(name, &event_id)| (name.clone(), event_id))
                        .collect();
                    if let ElementKind::TextInput {
                        text_cell: Some(source_text_cell),
                        ..
                    } = kind
                    {
                        text_source_cell = Some(*source_text_cell);
                    }
                    break;
                }
                _ => {}
            }
        }

        if let (Some(target_text_cell), Some(source_text_cell)) = (
            self.name_to_cell
                .get(&format!("{target_name}.text"))
                .copied(),
            text_source_cell,
        ) {
            self.nodes.push(IrNode::Derived {
                cell: target_text_cell,
                expr: IrExpr::CellRead(source_text_cell),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CellId, ElementKind, IrExpr, IrNode, IrPattern, IrValue, TextSegment, lower};
    use crate::platform::browser::engine_wasm::parse_source;
    use crate::platform::browser::kernel::{
        KernelValue, LatestCandidate, TickId, TickSeq, select_latest,
    };
    use std::fs;

    #[test]
    fn lower_scene_preserves_light_constructors() {
        let ast = parse_source(
            r#"
scene: Scene/new(
    root: Scene/Element/text(
        element: []
        style: []
        text: TEXT { hi }
    )
    lights: LIST {
        Light/directional(
            azimuth: 30
            altitude: 45
            spread: 1
            intensity: 1.2
            color: Oklch[lightness: 0.98, chroma: 0.015, hue: 65]
        )
        Light/ambient(
            intensity: 0.4
            color: Oklch[lightness: 0.8, chroma: 0.01, hue: 220]
        )
        Light/spot(
            target: FocusedElement
            color: Oklch[lightness: 0.7, chroma: 0.1, hue: 220]
            intensity: 0.3
            radius: 60
            softness: 0.85
        )
    }

    geometry: [
        bevel_angle: 45
    ]
)
"#,
        )
        .expect("parse");
        let program = lower(&ast, None).expect("lower");
        let render_root = program.render_root().expect("render root");
        let scene = render_root.scene.expect("scene handles");
        let lights_cell = scene.lights.expect("lights cell");
        let geometry_cell = scene.geometry.expect("geometry cell");

        let lights_items = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::Derived {
                    cell,
                    expr: IrExpr::ListConstruct(items),
                } if *cell == lights_cell => Some(items),
                _ => None,
            })
            .expect("lowered lights list");

        assert!(
            matches!(&lights_items[0], IrExpr::TaggedObject { tag, .. } if tag == "DirectionalLight")
        );
        assert!(
            matches!(&lights_items[1], IrExpr::TaggedObject { tag, .. } if tag == "AmbientLight")
        );
        assert!(matches!(&lights_items[2], IrExpr::TaggedObject { tag, .. } if tag == "SpotLight"));

        let geometry_fields = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::Derived {
                    cell,
                    expr: IrExpr::ObjectConstruct(fields),
                } if *cell == geometry_cell => Some(fields),
                _ => None,
            })
            .expect("lowered geometry object");

        assert!(
            geometry_fields
                .iter()
                .any(|(name, _)| name == "bevel_angle")
        );
    }

    #[test]
    fn shopping_list_input_latest_reads_change_text_and_clear_trigger() {
        let source = fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../playground/frontend/src/examples/shopping_list/shopping_list.bn"
        ))
        .expect("read shopping_list example");
        let ast = parse_source(&source).expect("parse");
        let program = lower(&ast, None).expect("lower");

        let change_text_cell = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, info)| {
                (info.name == "store.elements.item_input.event.change.text")
                    .then_some(CellId(idx as u32))
            })
            .expect("change text cell");
        let text_to_add_cell = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, info)| {
                (info.name == "store.text_to_add").then_some(CellId(idx as u32))
            })
            .expect("text_to_add cell");
        let item_input_text_cell = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, info)| {
                (info.name == "store.elements.item_input.text").then_some(CellId(idx as u32))
            })
            .expect("item input text cell");
        let latest_expr_cell = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::Derived {
                    cell,
                    expr: IrExpr::CellRead(source),
                } if *cell == item_input_text_cell => Some(*source),
                _ => None,
            })
            .expect("latest expr cell");
        let local_change_text_cell = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, info)| {
                (info.name == "element.event.change.text").then_some(CellId(idx as u32))
            })
            .expect("local change text cell");

        let latest = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::Latest { target, arms } if *target == latest_expr_cell => Some(arms),
                _ => None,
            })
            .expect("latest node");

        assert!(
            latest.iter().any(
                |arm| matches!(arm.body, IrExpr::CellRead(cell) if cell == local_change_text_cell)
            ),
            "LATEST should read the local element.event.change.text arm",
        );
        assert!(
            latest.iter().any(|arm| {
                matches!(arm.body, IrExpr::Constant(IrValue::Text(ref text)) if text.is_empty())
            }),
            "LATEST should keep the empty-text arm",
        );
        assert!(
            latest.iter().any(|arm| {
                arm.trigger.is_some()
                    && matches!(arm.body, IrExpr::Constant(IrValue::Text(ref text)) if text.is_empty())
            }),
            "LATEST should clear the input after the text_to_add trigger fires",
        );

        let text_to_add_is_event_driven = program.nodes.iter().any(|node| match node {
            IrNode::When { cell, .. } => *cell == text_to_add_cell,
            IrNode::Hold { cell, .. } => *cell == text_to_add_cell,
            IrNode::Latest { target, .. } => *target == text_to_add_cell,
            _ => false,
        });
        assert!(
            text_to_add_is_event_driven,
            "expected lowered store.text_to_add node"
        );
        assert_ne!(
            local_change_text_cell, change_text_cell,
            "linked text input lowers distinct local and linked change.text cells"
        );
    }

    fn collect_function_calls(expr: &IrExpr, out: &mut Vec<u32>) {
        match expr {
            IrExpr::FunctionCall { func, args } => {
                out.push(func.0);
                for arg in args {
                    collect_function_calls(arg, out);
                }
            }
            IrExpr::BinOp { lhs, rhs, .. } | IrExpr::Compare { lhs, rhs, .. } => {
                collect_function_calls(lhs, out);
                collect_function_calls(rhs, out);
            }
            IrExpr::UnaryNeg(inner) | IrExpr::Not(inner) => collect_function_calls(inner, out),
            IrExpr::TextConcat(parts) => {
                for part in parts {
                    if let TextSegment::Expr(expr) = part {
                        collect_function_calls(expr, out);
                    }
                }
            }
            IrExpr::FieldAccess { object, .. } => collect_function_calls(object, out),
            IrExpr::ObjectConstruct(fields) | IrExpr::TaggedObject { fields, .. } => {
                for (_, value) in fields {
                    collect_function_calls(value, out);
                }
            }
            IrExpr::ListConstruct(items) => {
                for item in items {
                    collect_function_calls(item, out);
                }
            }
            IrExpr::PatternMatch { arms, .. } => {
                for (_, body) in arms {
                    collect_function_calls(body, out);
                }
            }
            IrExpr::Constant(_) | IrExpr::CellRead(_) => {}
        }
    }

    #[test]
    fn cells_runtime_function_calls_are_limited_to_recursive_helpers() {
        let source = fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../playground/frontend/src/examples/cells/cells.bn"
        ))
        .expect("read cells example");
        let ast = parse_source(&source).expect("parse");
        let program = lower(&ast, None).expect("lower");

        let mut calls = Vec::new();
        for node in &program.nodes {
            match node {
                IrNode::Derived { expr, .. } => collect_function_calls(expr, &mut calls),
                IrNode::When { arms, .. } | IrNode::While { arms, .. } => {
                    for (_, body) in arms {
                        collect_function_calls(body, &mut calls);
                    }
                }
                IrNode::Latest { arms, .. } => {
                    for arm in arms {
                        collect_function_calls(&arm.body, &mut calls);
                    }
                }
                _ => {}
            }
        }

        let mut names: Vec<String> = calls
            .into_iter()
            .filter_map(|func_id| {
                program
                    .functions
                    .get(func_id as usize)
                    .map(|func| func.name.clone())
            })
            .collect();
        names.sort();
        names.dedup();

        assert!(
            !names.iter().any(|name| matches!(
                name.as_str(),
                "make_row"
                    | "make_cell_element"
                    | "compute_value"
                    | "cell_formula"
                    | "expression_value"
            )),
            "non-recursive cells helpers should inline in Wasm, got runtime calls: {:?}",
            names
        );
    }

    #[test]
    fn reduced_timer_duration_row_does_not_self_reference_shadowed_passed_alias() {
        let ast = parse_source(
            r#"
store: [
    elements: [
        duration_slider: LINK
        reset_button: LINK
    ]
    max_duration: 15 |> HOLD state {
        elements.duration_slider.event.change.value
    }
]

document: Document/new(root: root_element(PASS: [store: store]))

FUNCTION root_element() {
    BLOCK {
        max_duration: PASSED.store.max_duration
        Element/stripe(
            element: []
            direction: Column
            gap: 16
            style: [padding: 20, width: 400]
            items: LIST {
                duration_row(PASS: [store: PASSED.store, max_duration: max_duration])
                Element/button(
                    element: [event: [press: LINK]]
                    style: [width: Fill]
                    label: TEXT { Reset }
                ) |> LINK { PASSED.store.elements.reset_button }
            }
        )
    }
}

FUNCTION duration_row() {
    BLOCK {
        max_duration: PASSED.max_duration
        Element/stripe(
            element: []
            direction: Row
            gap: 10
            style: []
            items: LIST {
                Element/label(element: [], style: [], label: TEXT { Duration: })
                Element/slider(
                    element: [event: [change: LINK]]
                    style: [width: 200]
                    label: Hidden[text: TEXT { Duration }]
                    value: max_duration
                    min: 1
                    max: 30
                    step: 0.1
                ) |> LINK { PASSED.store.elements.duration_slider }
                Element/label(
                    element: []
                    style: []
                    label: TEXT { {max_duration}s }
                )
            }
        )
    }
}
"#,
        )
        .expect("parse");
        let program = lower(&ast, None).expect("lower");

        let max_duration_cells: Vec<u32> = program
            .cells
            .iter()
            .enumerate()
            .filter_map(|(idx, cell)| (cell.name == "max_duration").then_some(idx as u32))
            .collect();

        assert!(
            max_duration_cells.len() >= 2,
            "expected both outer and inner max_duration locals"
        );

        for cell_id in max_duration_cells {
            if let Some(IrNode::Derived {
                cell,
                expr: IrExpr::CellRead(source),
            }) = program.nodes.iter().find(|node| match node {
                IrNode::Derived { cell, .. } => cell.0 == cell_id,
                _ => false,
            }) {
                assert_ne!(
                    cell.0, source.0,
                    "shadowed PASSED alias lowered to a self-reference for cell {}",
                    cell.0
                );
            }
        }

        let slider_change_event = program.nodes.iter().find_map(|node| match node {
            IrNode::Element { kind, links, .. }
                if matches!(kind, super::ElementKind::Slider { .. }) =>
            {
                links
                    .iter()
                    .find(|(name, _)| name == "change")
                    .map(|(_, event)| *event)
            }
            _ => None,
        });
        let max_duration_trigger = program.nodes.iter().find_map(|node| match node {
            IrNode::Hold {
                cell,
                trigger_bodies,
                ..
            } if program.cells[cell.0 as usize]
                .name
                .ends_with("max_duration") =>
            {
                trigger_bodies.first().map(|(event, _)| *event)
            }
            _ => None,
        });
        let max_duration_trigger_body = program.nodes.iter().find_map(|node| match node {
            IrNode::Hold {
                cell,
                trigger_bodies,
                ..
            } if program.cells[cell.0 as usize]
                .name
                .ends_with("max_duration") =>
            {
                trigger_bodies.first().map(|(_, body)| body.clone())
            }
            _ => None,
        });

        assert_eq!(
            slider_change_event.map(|event| event.0),
            max_duration_trigger.map(|event| event.0),
            "slider change event should drive the max_duration HOLD (slider={:?}, hold={:?})",
            slider_change_event.map(|event| program.events[event.0 as usize].name.clone()),
            max_duration_trigger.map(|event| program.events[event.0 as usize].name.clone())
        );

        assert!(
            matches!(
                max_duration_trigger_body,
                Some(IrExpr::CellRead(cell))
                    if program.cells[cell.0 as usize]
                        .name
                        .ends_with(".event.change.value")
            ),
            "max_duration HOLD should read the slider change.value payload, got {:?}",
            max_duration_trigger_body
        );

        assert!(
            program
                .cells
                .iter()
                .any(|cell| cell.name.ends_with(".event.change.value")),
            "slider LINK should allocate a change.value payload cell"
        );
    }

    #[test]
    fn full_timer_slider_change_event_and_payload_are_wired_to_max_duration_hold() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/timer/timer.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");
        let timer_count = program
            .nodes
            .iter()
            .filter(|node| matches!(node, IrNode::Timer { .. }))
            .count();

        let (slider_change_event, slider_value_cell) = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::Element { kind, links, .. } => match kind {
                    super::ElementKind::Slider { value_cell, .. } => Some((
                        links
                            .iter()
                            .find(|(name, _)| name == "change")
                            .map(|(_, event)| *event),
                        *value_cell,
                    )),
                    _ => None,
                },
                _ => None,
            })
            .expect("timer slider not found");
        let max_duration_hold = program.nodes.iter().find_map(|node| match node {
            IrNode::Hold {
                cell,
                trigger_bodies,
                ..
            } if program.cells[cell.0 as usize].name == "store.max_duration" => trigger_bodies
                .first()
                .map(|(event, body)| (*event, body.clone())),
            _ => None,
        });

        let Some((hold_event, hold_body)) = max_duration_hold else {
            panic!("store.max_duration HOLD not found");
        };

        assert_eq!(
            timer_count, 1,
            "timer example should lower to exactly one timer node"
        );
        assert_eq!(
            slider_change_event.map(|event| event.0),
            Some(hold_event.0),
            "timer slider and store.max_duration HOLD should share the same change event"
        );
        assert!(
            matches!(
                slider_value_cell,
                Some(cell) if program.cells[cell.0 as usize].name.ends_with("max_duration")
            ),
            "timer slider value should read max_duration, got {:?}",
            slider_value_cell.map(|cell| program.cells[cell.0 as usize].name.clone())
        );
        assert!(
            matches!(
                hold_body,
                IrExpr::CellRead(cell)
                    if program.cells[cell.0 as usize]
                        .name
                        .ends_with(".event.change.value")
            ),
            "store.max_duration HOLD should read change.value, got {:?}",
            hold_body
        );
    }

    #[test]
    fn crud_filter_input_text_property_tracks_text_argument_cell() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/crud/crud.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");

        let filter_text_cell = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| {
                (cell.name == "store.elements.filter_input.text").then_some(CellId(idx as u32))
            })
            .expect("filter input .text cell");

        let source_name = program.nodes.iter().find_map(|node| match node {
            IrNode::Derived {
                cell,
                expr: IrExpr::CellRead(source),
            } if *cell == filter_text_cell => Some(program.cells[source.0 as usize].name.clone()),
            _ => None,
        });

        assert!(
            source_name.is_some(),
            "filter_input.text should be wired to the text argument cell, not left as Void"
        );
    }

    #[test]
    fn crud_create_button_press_is_shared_by_person_to_add_and_list_append() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/crud/crud.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");

        let create_press_event = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::Element { kind, links, .. } => match kind {
                    ElementKind::Button { label, .. } => {
                        let is_create = matches!(
                            label,
                            IrExpr::Constant(IrValue::Text(text)) if text == "Create"
                        ) || matches!(
                            label,
                            IrExpr::TextConcat(parts)
                                if parts.len() == 1
                                    && matches!(&parts[0], TextSegment::Literal(text) if text == "Create")
                        );
                        is_create.then(|| {
                            links
                                .iter()
                                .find(|(name, _)| name == "press")
                                .map(|(_, event)| *event)
                        })?
                    }
                    _ => None,
                },
                _ => None,
            })
            .expect("Create button press event");

        let person_to_add_trigger = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::Then { cell, trigger, .. }
                    if program.cells[cell.0 as usize].name == "store.person_to_add" =>
                {
                    Some(*trigger)
                }
                _ => None,
            })
            .expect("store.person_to_add THEN");

        let append_trigger = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::ListAppend { trigger, item, .. }
                    if program.cells[item.0 as usize].name == "store.person_to_add" =>
                {
                    Some(*trigger)
                }
                _ => None,
            })
            .expect("CRUD ListAppend trigger");

        assert_eq!(
            person_to_add_trigger, create_press_event,
            "store.person_to_add should fire on the Create button press event"
        );
        assert_eq!(
            append_trigger, create_press_event,
            "CRUD ListAppend should reuse the Create button press event"
        );
    }

    #[test]
    fn crud_person_to_add_fields_read_the_name_and_surname_inputs() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/crud/crud.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");

        let name_input_text = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| {
                (cell.name == "store.elements.name_input.text").then_some(CellId(idx as u32))
            })
            .expect("name input text cell");
        let surname_input_text = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| {
                (cell.name == "store.elements.surname_input.text").then_some(CellId(idx as u32))
            })
            .expect("surname input text cell");
        let person_to_add = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| {
                (cell.name == "store.person_to_add").then_some(CellId(idx as u32))
            })
            .expect("store.person_to_add cell");
        let fields = program
            .cell_field_cells
            .get(&person_to_add)
            .expect("store.person_to_add fields");
        let name_field = fields
            .get("name")
            .copied()
            .expect("person_to_add.name field");
        let surname_field = fields
            .get("surname")
            .copied()
            .expect("person_to_add.surname field");

        let name_init = program.nodes.iter().find_map(|node| match node {
            IrNode::Hold { cell, init, .. } if *cell == name_field => Some(init),
            _ => None,
        });
        let surname_init = program.nodes.iter().find_map(|node| match node {
            IrNode::Hold { cell, init, .. } if *cell == surname_field => Some(init),
            _ => None,
        });
        let name_param_source = match name_init {
            Some(IrExpr::CellRead(param_cell)) => {
                program.nodes.iter().find_map(|node| match node {
                    IrNode::Derived {
                        cell,
                        expr: IrExpr::CellRead(source),
                    } if *cell == *param_cell => Some(*source),
                    _ => None,
                })
            }
            _ => None,
        };
        let surname_param_source = match surname_init {
            Some(IrExpr::CellRead(param_cell)) => {
                program.nodes.iter().find_map(|node| match node {
                    IrNode::Derived {
                        cell,
                        expr: IrExpr::CellRead(source),
                    } if *cell == *param_cell => Some(*source),
                    _ => None,
                })
            }
            _ => None,
        };

        assert!(
            matches!(name_param_source, Some(cell) if cell == name_input_text),
            "person_to_add.name should read the name input text cell, got init={:?} source={:?}",
            name_init.map(|expr| match expr {
                IrExpr::CellRead(cell) =>
                    format!("CellRead({})", program.cells[cell.0 as usize].name),
                other => format!("{other:?}"),
            }),
            name_param_source.map(|cell| program.cells[cell.0 as usize].name.clone())
        );
        assert!(
            matches!(surname_param_source, Some(cell) if cell == surname_input_text),
            "person_to_add.surname should read the surname input text cell, got init={:?} source={:?}",
            surname_init.map(|expr| match expr {
                IrExpr::CellRead(cell) =>
                    format!("CellRead({})", program.cells[cell.0 as usize].name),
                other => format!("{other:?}"),
            }),
            surname_param_source.map(|cell| program.cells[cell.0 as usize].name.clone())
        );
    }

    #[test]
    fn crud_person_row_press_is_shared_by_selected_id_updates() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/crud/crud.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");

        let row_press_event = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::Element { kind, links, .. } => match kind {
                    ElementKind::Button { label, .. } if matches!(label, IrExpr::CellRead(_)) => {
                        links
                            .iter()
                            .find(|(name, _)| name == "press")
                            .map(|(_, event)| *event)
                    }
                    _ => None,
                },
                _ => None,
            })
            .expect("person row press event");

        let selected_id_triggers = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::Hold {
                    cell,
                    trigger_bodies,
                    ..
                } if program.cells[cell.0 as usize].name == "store.selected_id" => Some(
                    trigger_bodies
                        .iter()
                        .map(|(event, _)| *event)
                        .collect::<Vec<_>>(),
                ),
                _ => None,
            })
            .expect("store.selected_id HOLD");

        let describe_event = |event: super::EventId| {
            let info = &program.events[event.0 as usize];
            format!("EventId({}) {} {:?}", event.0, info.name, info.source)
        };

        assert!(
            selected_id_triggers.contains(&row_press_event),
            "store.selected_id should react to person row press events, got {:?} vs {}",
            selected_id_triggers
                .iter()
                .map(|event| describe_event(*event))
                .collect::<Vec<_>>(),
            describe_event(row_press_event)
        );
    }

    #[test]
    fn crud_selected_id_preserves_object_field_cells() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/crud/crud.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");

        let selected_id = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| {
                (cell.name == "store.selected_id").then_some(CellId(idx as u32))
            })
            .expect("store.selected_id cell");

        let selected_fields = program
            .cell_field_cells
            .get(&selected_id)
            .expect("store.selected_id field map");
        let id_cell = *selected_fields.get("id").expect("selected_id.id field");
        println!(
            "selected_id.id leaf: {} {:?}",
            program.cells[id_cell.0 as usize].name,
            program.nodes.iter().find(|node| match node {
                IrNode::Derived { cell, .. }
                | IrNode::PipeThrough { cell, .. }
                | IrNode::Then { cell, .. }
                | IrNode::Hold { cell, .. }
                | IrNode::Latest { target: cell, .. }
                | IrNode::When { cell, .. }
                | IrNode::While { cell, .. }
                | IrNode::CustomCall { cell, .. } => *cell == id_cell,
                _ => false,
            })
        );
        assert!(
            id_cell != selected_id,
            "selected_id.id should point to a distinct scalar leaf cell"
        );
    }

    #[test]
    fn latest_press_sequence_lowers_with_expected_latest_arms() {
        let ast = parse_source(
            r#"
left_button: LINK
right_button: LINK

selected: LATEST {
    left_button.event.press |> THEN { TEXT { left } }
    right_button.event.press |> THEN { TEXT { right } }
}

document: Document/new(root:
    Element/stripe(
        element: []
        direction: Column
        gap: 0
        style: []
        items: LIST {
            Element/button(
                element: [event: [press: LINK]]
                label: TEXT { Left }
                style: []
            ) |> LINK { left_button }
            Element/button(
                element: [event: [press: LINK]]
                label: TEXT { Right }
                style: []
            ) |> LINK { right_button }
            Element/label(
                element: []
                style: []
                label: selected
            )
        }
    )
)
"#,
        )
        .expect("parse");
        let program = lower(&ast, None).expect("lower");

        let selected = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| (cell.name == "selected").then_some(CellId(idx as u32)))
            .expect("selected cell");

        let arms = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::Latest { target, arms } if *target == selected => Some(arms.clone()),
                _ => None,
            })
            .expect("selected latest node");

        assert_eq!(arms.len(), 2, "expected exactly two latest arms");

        let mut arm_values = Vec::new();
        let trigger_names: Vec<String> = arms
            .iter()
            .map(|arm| {
                arm_values.push(match &arm.body {
                    IrExpr::Constant(IrValue::Text(text)) => text.clone(),
                    IrExpr::TextConcat(parts) if parts.len() == 1 => match &parts[0] {
                        TextSegment::Literal(text) => text.clone(),
                        other => {
                            panic!("expected latest arm body text literal segment, got {other:?}")
                        }
                    },
                    other => panic!("expected latest arm body to be text constant, got {other:?}"),
                });
                let trigger = arm.trigger.expect("LATEST arm trigger");
                program.events[trigger.0 as usize].name.clone()
            })
            .collect();

        assert_eq!(arm_values, vec!["left".to_string(), "right".to_string()]);
        assert!(
            trigger_names.iter().all(|name| !name.is_empty()),
            "expected selected LATEST arms to keep concrete trigger bindings, got {trigger_names:?}"
        );
        assert!(
            trigger_names.iter().all(|name| name == "latest_trigger"),
            "expected selected LATEST lowering to keep per-arm synthetic triggers consistently, got {trigger_names:?}"
        );

        let expected = select_latest(&[
            LatestCandidate::new(KernelValue::from("left"), TickSeq::new(TickId(1), 1)),
            LatestCandidate::new(KernelValue::from("right"), TickSeq::new(TickId(1), 2)),
        ]);
        assert_eq!(expected, KernelValue::from("right"));
    }

    #[test]
    fn hold_press_sequence_lowers_to_single_press_trigger_body() {
        let ast = parse_source(
            r#"
increment_button: LINK

counter: 0 |> HOLD state {
    increment_button.event.press |> THEN { state + 1 }
}

document: Document/new(root:
    Element/button(
        element: [event: [press: LINK]]
        label: TEXT { Counter }
        style: []
    ) |> LINK { increment_button }
)
"#,
        )
        .expect("parse");
        let program = lower(&ast, None).expect("lower");

        let counter = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| (cell.name == "counter").then_some(CellId(idx as u32)))
            .expect("counter cell");

        let hold_cell = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::Hold { cell, .. } if *cell == counter => Some(*cell),
                IrNode::StreamSkip { cell, source, .. } if *cell == counter => Some(*source),
                _ => None,
            })
            .expect("counter HOLD source");

        let Some(IrNode::Hold {
            cell,
            init,
            trigger_bodies,
        }) = program
            .nodes
            .iter()
            .find(|node| matches!(node, IrNode::Hold { cell, .. } if *cell == hold_cell))
        else {
            panic!("expected counter HOLD node");
        };

        assert_eq!(*cell, hold_cell);
        assert!(
            matches!(init, IrExpr::Constant(IrValue::Number(n)) if (*n - 0.0).abs() < f64::EPSILON),
            "expected HOLD init to remain literal 0, got {init:?}"
        );
        assert_eq!(
            trigger_bodies.len(),
            1,
            "expected one trigger body for press-driven HOLD"
        );

        let trigger = trigger_bodies[0].0;
        let trigger_name = &program.events[trigger.0 as usize].name;
        assert_eq!(
            trigger_name, "hold_trigger",
            "expected HOLD lowering to keep a stable synthetic trigger name"
        );
        assert!(
            matches!(
                &trigger_bodies[0].1,
                IrExpr::BinOp {
                    op: super::super::ir::BinOp::Add,
                    lhs,
                    rhs,
                }
                if matches!(&**lhs, IrExpr::CellRead(read) if *read == hold_cell)
                    && matches!(&**rhs, IrExpr::Constant(IrValue::Number(n)) if (*n - 1.0).abs() < f64::EPSILON)
            ),
            "expected HOLD body to be `state + 1` over the HOLD cell, got {:?}",
            trigger_bodies[0].1
        );
    }

    #[test]
    fn link_press_event_rebinds_element_links_to_the_target_variable() {
        let ast = parse_source(
            r#"
increment_button: LINK

document: Document/new(root:
    Element/button(
        element: [event: [press: LINK]]
        label: TEXT { Increment }
        style: []
    ) |> LINK { increment_button }
)

pressed: increment_button.event.press |> THEN { TEXT { pressed } }
"#,
        )
        .expect("parse");
        let program = lower(&ast, None).expect("lower");

        let pressed = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| (cell.name == "pressed").then_some(CellId(idx as u32)))
            .expect("pressed cell");
        let pressed_trigger = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::Then { cell, trigger, .. } if *cell == pressed => Some(*trigger),
                _ => None,
            })
            .expect("pressed THEN trigger");

        let (button_links, button_kind) = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::Element { kind, links, .. } => Some((links.clone(), kind)),
                _ => None,
            })
            .expect("button element node");
        assert!(
            matches!(button_kind, &ElementKind::Button { .. }),
            "expected linked element to stay a button"
        );

        let press_event = button_links
            .iter()
            .find_map(|(name, event)| (name == "press").then_some(*event))
            .expect("button press link");
        assert_eq!(
            press_event, pressed_trigger,
            "expected LINK rebinding to share the same press EventId between element and consumer"
        );
        assert_eq!(
            program.events[press_event.0 as usize].name, "element.press",
            "expected linked press event to keep concrete press name in Wasm IR"
        );
    }

    #[test]
    fn cells_edit_started_latest_preserves_double_click_triggers() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/cells/cells.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");

        let edit_started = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| (cell.name == "edit_started").then_some(CellId(idx as u32)))
            .expect("edit_started cell");

        let arms = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::Latest { target, arms } if *target == edit_started => Some(arms.clone()),
                _ => None,
            })
            .expect("edit_started latest node");

        let trigger_names: Vec<String> = arms
            .iter()
            .filter_map(|arm| {
                arm.trigger
                    .and_then(|event| program.events.get(event.0 as usize))
                    .map(|event| event.name.clone())
            })
            .collect();

        assert!(
            arms.iter().all(|arm| arm.trigger.is_some()),
            "expected edit_started to preserve concrete triggers for every arm, got {trigger_names:?}"
        );
        assert!(
            trigger_names
                .iter()
                .all(|name| name == "element.double_click"),
            "expected edit_started to preserve real double_click triggers, got {trigger_names:?}"
        );
    }

    #[test]
    fn cells_enter_pressed_latest_preserves_key_down_when_nodes() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/cells/cells.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");

        let enter_pressed = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| (cell.name == "enter_pressed").then_some(CellId(idx as u32)))
            .expect("enter_pressed cell");

        let outer_arms = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::Latest { target, arms } if *target == enter_pressed => Some(arms.clone()),
                _ => None,
            })
            .expect("enter_pressed latest node");

        let mut inner_pipe_results = Vec::new();
        for outer_arm in outer_arms {
            let IrExpr::CellRead(row_enter_pressed) = outer_arm.body else {
                continue;
            };
            let Some(row_arms) = program.nodes.iter().find_map(|node| match node {
                IrNode::Latest { target, arms } if *target == row_enter_pressed => Some(arms),
                _ => None,
            }) else {
                continue;
            };
            for arm in row_arms {
                if let IrExpr::CellRead(cell) = arm.body {
                    inner_pipe_results.push(cell);
                }
            }
        }

        assert!(
            !inner_pipe_results.is_empty(),
            "expected enter_pressed to expose inner row pipe results"
        );

        for cell in inner_pipe_results {
            let node = program.nodes.iter().find(|node| match node {
                IrNode::Derived { cell: c, .. }
                | IrNode::PipeThrough { cell: c, .. }
                | IrNode::Then { cell: c, .. }
                | IrNode::Hold { cell: c, .. }
                | IrNode::Latest { target: c, .. }
                | IrNode::When { cell: c, .. }
                | IrNode::While { cell: c, .. }
                | IrNode::CustomCall { cell: c, .. }
                | IrNode::TextInterpolation { cell: c, .. }
                | IrNode::MathSum { cell: c, .. }
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
                | IrNode::TextStartsWith { cell: c, .. }
                | IrNode::MathRound { cell: c, .. }
                | IrNode::MathMin { cell: c, .. }
                | IrNode::MathMax { cell: c, .. }
                | IrNode::StreamSkip { cell: c, .. }
                | IrNode::HoldLoop { cell: c, .. } => *c == cell,
                IrNode::Element { cell: c, .. } => *c == cell,
                IrNode::Document { .. } | IrNode::Timer { .. } => false,
            });

            match node {
                Some(IrNode::When { source, arms, .. }) => {
                    let source_name = &program.cells[source.0 as usize].name;
                    assert!(
                        source_name.contains("editing.event.key_down.key"),
                        "expected enter_pressed WHEN source to be key_down.key, got {source_name}"
                    );
                    assert!(
                        arms.iter().any(|(pattern, body)| {
                            matches!(pattern, IrPattern::Tag(tag) if tag == "Enter")
                                && !matches!(body, IrExpr::Constant(IrValue::Void))
                        }),
                        "expected an Enter arm with non-void body for {}, got {arms:?}",
                        program.cells[cell.0 as usize].name
                    );
                }
                other => panic!(
                    "expected enter_pressed pipe result {} to lower to a WHEN node, got {other:?}",
                    program.cells[cell.0 as usize].name
                ),
            }
        }
    }

    #[test]
    fn minimal_row_cell_helper_preserves_nested_double_click_event() {
        let source = r#"
FUNCTION make_cell(column, row) { [row: row, column: column, cell_elements: [display: LINK, editing: LINK]] }
FUNCTION row_cell(row_cells, column) { row_cells |> List/get(index: column) }
FUNCTION make_cell_element(cell) {
    False |> WHILE {
        True => Element/text_input(element: [event: [change: LINK]] text: Text/empty()) |> LINK { cell.cell_elements.editing }
        False => Element/label(element: [event: [double_click: LINK]], label: TEXT { x }) |> LINK { cell.cell_elements.display }
    }
}
FUNCTION edit_started_from_cell(cell) { cell.cell_elements.display.event.double_click |> THEN { [row: cell.row, column: cell.column] } }
row_1_cells: LIST { make_cell(column: 1, row: 1) }
document: Document/new(root: make_cell_element(cell: row_cell(row_cells: row_1_cells, column: 1)))
edit_started: edit_started_from_cell(cell: row_cell(row_cells: row_1_cells, column: 1))
"#;

        let ast = parse_source(source).expect("parse");
        let program = lower(&ast, None).expect("lower");

        let edit_started = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| (cell.name == "edit_started").then_some(CellId(idx as u32)))
            .expect("edit_started cell");

        let then_trigger = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::Then { cell, trigger, .. } if *cell == edit_started => Some(*trigger),
                _ => None,
            })
            .or_else(|| {
                program.nodes.iter().find_map(|node| match node {
                    IrNode::Derived {
                        cell,
                        expr: IrExpr::CellRead(source),
                    } if *cell == edit_started => {
                        program.nodes.iter().find_map(|inner| match inner {
                            IrNode::Then { cell, trigger, .. } if *cell == *source => {
                                Some(*trigger)
                            }
                            _ => None,
                        })
                    }
                    _ => None,
                })
            })
            .unwrap_or_else(|| {
                panic!(
                    "edit_started then trigger; node={:?}",
                    program.nodes.iter().find(|node| match node {
                        IrNode::Derived { cell, .. }
                        | IrNode::PipeThrough { cell, .. }
                        | IrNode::Then { cell, .. }
                        | IrNode::Hold { cell, .. }
                        | IrNode::Latest { target: cell, .. }
                        | IrNode::When { cell, .. }
                        | IrNode::While { cell, .. }
                        | IrNode::CustomCall { cell, .. }
                        | IrNode::TextInterpolation { cell, .. }
                        | IrNode::MathSum { cell, .. }
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
                        | IrNode::StreamSkip { cell, .. }
                        | IrNode::HoldLoop { cell, .. } => *cell == edit_started,
                        IrNode::Element { cell, .. } => *cell == edit_started,
                        IrNode::Document { .. } | IrNode::Timer { .. } => false,
                    })
                )
            });

        let event_name = &program.events[then_trigger.0 as usize].name;
        assert_eq!(event_name, "object.cell_elements.display.double_click");
    }

    #[test]
    fn minimal_row_data_cells_latest_preserves_nested_double_click_event() {
        let source = r#"
FUNCTION make_cell(column, row) { [row: row, column: column, cell_elements: [display: LINK, editing: LINK]] }
FUNCTION make_row_data(row_number, row_cells) { [row: row_number, cells: row_cells] }
FUNCTION row_cell(row_cells, column) { row_cells |> List/get(index: column) }
FUNCTION make_cell_element(cell) {
    False |> WHILE {
        True => Element/text_input(element: [event: [change: LINK]] text: Text/empty()) |> LINK { cell.cell_elements.editing }
        False => Element/label(element: [event: [double_click: LINK]], label: TEXT { x }) |> LINK { cell.cell_elements.display }
    }
}
FUNCTION edit_started_from_cell(cell) { cell.cell_elements.display.event.double_click |> THEN { [row: cell.row, column: cell.column] } }
FUNCTION edit_started_in_row(row_cells) { edit_started_from_cell(cell: row_cell(row_cells: row_cells, column: 1)) }
row_1_cells: LIST { make_cell(column: 1, row: 1) }
all_row_cells: LIST { make_row_data(row_number: 1, row_cells: row_1_cells) }
document: Document/new(root: make_cell_element(cell: row_cell(row_cells: row_1_cells, column: 1)))
edit_started: all_row_cells |> List/map(row_data, new: edit_started_in_row(row_cells: row_data.cells)) |> List/latest()
"#;

        let ast = parse_source(source).expect("parse");
        let program = lower(&ast, None).expect("lower");

        let edit_started = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| (cell.name == "edit_started").then_some(CellId(idx as u32)))
            .expect("edit_started cell");

        let latest_arms = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::Latest { target, arms } if *target == edit_started => Some(arms),
                _ => None,
            })
            .expect("edit_started latest");

        let then_trigger = latest_arms[0]
            .trigger
            .expect("row-level latest arm trigger");

        let event_name = &program.events[then_trigger.0 as usize].name;
        assert_eq!(event_name, "object.cell_elements.display.double_click");
    }

    #[test]
    fn minimal_row_data_cells_latest_preserves_nested_editing_events() {
        let source = r#"
FUNCTION make_cell(column, row) { [row: row, column: column, cell_elements: [display: LINK, editing: LINK]] }
FUNCTION make_row_data(row_number, row_cells) { [row: row_number, cells: row_cells] }
FUNCTION row_cell(row_cells, column) { row_cells |> List/get(index: column) }
FUNCTION make_cell_element(cell) {
    True |> WHILE {
        True => Element/text_input(element: [event: [change: LINK, key_down: LINK, blur: LINK]] text: Text/empty()) |> LINK { cell.cell_elements.editing }
        False => Element/label(element: [event: [double_click: LINK]], label: TEXT { x }) |> LINK { cell.cell_elements.display }
    }
}
FUNCTION edit_changed_from_cell(cell) { cell.cell_elements.editing.event.change |> THEN { [text: cell.cell_elements.editing.event.change.text] } }
FUNCTION enter_pressed_from_cell(cell) { cell.cell_elements.editing.event.key_down.key |> WHEN { Enter => [row: cell.row, column: cell.column, text: cell.cell_elements.editing.event.key_down.text] __ => SKIP } }
FUNCTION edit_changed_in_row(row_cells) { edit_changed_from_cell(cell: row_cell(row_cells: row_cells, column: 1)) }
FUNCTION enter_pressed_in_row(row_cells) { enter_pressed_from_cell(cell: row_cell(row_cells: row_cells, column: 1)) }
row_1_cells: LIST { make_cell(column: 1, row: 1) }
all_row_cells: LIST { make_row_data(row_number: 1, row_cells: row_1_cells) }
document: Document/new(root: make_cell_element(cell: row_cell(row_cells: row_1_cells, column: 1)))
edit_changed: all_row_cells |> List/map(row_data, new: edit_changed_in_row(row_cells: row_data.cells)) |> List/latest()
enter_pressed: all_row_cells |> List/map(row_data, new: enter_pressed_in_row(row_cells: row_data.cells)) |> List/latest()
"#;

        let ast = parse_source(source).expect("parse");
        let program = lower(&ast, None).expect("lower");

        let edit_changed = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| (cell.name == "edit_changed").then_some(CellId(idx as u32)))
            .expect("edit_changed cell");
        let edit_changed_arms = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::Latest { target, arms } if *target == edit_changed => Some(arms),
                _ => None,
            })
            .expect("edit_changed latest");
        let change_trigger = edit_changed_arms[0]
            .trigger
            .expect("edit_changed latest arm trigger");
        assert_eq!(
            &program.events[change_trigger.0 as usize].name,
            "object.cell_elements.editing.change"
        );

        let enter_pressed = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| (cell.name == "enter_pressed").then_some(CellId(idx as u32)))
            .expect("enter_pressed cell");
        let row_enter = match program.nodes.iter().find(|node| match node {
            IrNode::Latest { target, .. } if *target == enter_pressed => true,
            IrNode::PipeThrough { cell, .. } if *cell == enter_pressed => true,
            _ => false,
        }) {
            Some(IrNode::Latest { arms, .. }) => match &arms[0].body {
                IrExpr::CellRead(cell) => *cell,
                other => panic!("expected enter row cell, got {other:?}"),
            },
            Some(IrNode::PipeThrough { source, .. }) => *source,
            other => panic!("enter_pressed lowering: {other:?}"),
        };

        let when_cell = match program.nodes.iter().find(|node| match node {
            IrNode::When { cell, .. } if *cell == row_enter => true,
            IrNode::ListMap { cell, .. } if *cell == row_enter => true,
            _ => false,
        }) {
            Some(IrNode::When { cell, .. }) => *cell,
            Some(IrNode::ListMap { template, .. }) => match template.as_ref() {
                IrNode::Derived {
                    expr: IrExpr::CellRead(cell),
                    ..
                } => *cell,
                other => panic!("unexpected enter ListMap template: {other:?}"),
            },
            other => panic!("unexpected enter row cell lowering: {other:?}"),
        };

        let row_when = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::When { cell, source, arms } if *cell == when_cell => Some((*source, arms)),
                _ => None,
            })
            .expect("row enter WHEN");
        assert!(row_when.1.iter().any(|(pattern, body)| {
            matches!(pattern, IrPattern::Tag(tag) if tag == "Enter")
                && !matches!(body, IrExpr::Constant(IrValue::Skip))
        }));
    }

    #[test]
    fn minimal_row_data_object_cells_preserves_nested_double_click_event() {
        let source = r#"
FUNCTION make_cell(column, row) { [row: row, column: column, cell_elements: [display: LINK, editing: LINK]] }
FUNCTION make_row_data(row_number, row_cells) { [row: row_number, cells: row_cells] }
FUNCTION row_cell(row_cells, column) { row_cells |> List/get(index: column) }
FUNCTION make_cell_element(cell) {
    False |> WHILE {
        True => Element/text_input(element: [event: [change: LINK, key_down: LINK, blur: LINK]] text: Text/empty()) |> LINK { cell.cell_elements.editing }
        False => Element/label(element: [event: [double_click: LINK]], label: TEXT { x }) |> LINK { cell.cell_elements.display }
    }
}
FUNCTION edit_started_from_cell(cell) { cell.cell_elements.display.event.double_click |> THEN { [row: cell.row, column: cell.column] } }
row_1_cells: LIST {
    make_cell(column: 1, row: 1)
    make_cell(column: 2, row: 1)
    make_cell(column: 3, row: 1)
}
row_data: make_row_data(row_number: 1, row_cells: row_1_cells)
document: Document/new(root: make_cell_element(cell: row_cell(row_cells: row_data.cells, column: 1)))
edit_started: edit_started_from_cell(cell: row_cell(row_cells: row_data.cells, column: 1))
"#;

        let ast = parse_source(source).expect("parse");
        let program = lower(&ast, None).expect("lower");

        let edit_started = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| (cell.name == "edit_started").then_some(CellId(idx as u32)))
            .expect("edit_started cell");

        let then_trigger = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::Then { cell, trigger, .. } if *cell == edit_started => Some(*trigger),
                IrNode::Derived {
                    cell,
                    expr: IrExpr::CellRead(source),
                } if *cell == edit_started => program.nodes.iter().find_map(|inner| match inner {
                    IrNode::Then { cell, trigger, .. } if *cell == *source => Some(*trigger),
                    _ => None,
                }),
                _ => None,
            })
            .expect("edit_started then trigger");

        assert_eq!(
            &program.events[then_trigger.0 as usize].name,
            "object.cell_elements.display.double_click"
        );
    }

    #[test]
    fn minimal_row_render_map_preserves_cell_events_for_edit_helpers() {
        let source = r#"
FUNCTION make_cell(column, row) { [row: row, column: column, cell_elements: [display: LINK, editing: LINK]] }
FUNCTION row_cell(row_cells, column) { row_cells |> List/get(index: column) }
FUNCTION make_cell_element(cell) {
    False |> WHILE {
        True => Element/text_input(element: [event: [change: LINK, key_down: LINK, blur: LINK]] text: Text/empty()) |> LINK { cell.cell_elements.editing }
        False => Element/label(element: [event: [double_click: LINK]], label: TEXT { x }) |> LINK { cell.cell_elements.display }
    }
}
FUNCTION make_row_elements(row_cells) { row_cells |> List/map(cell, new: make_cell_element(cell: cell)) }
FUNCTION edit_started_from_cell(cell) { cell.cell_elements.display.event.double_click |> THEN { [row: cell.row, column: cell.column] } }
FUNCTION edit_started_in_row(row_cells) { edit_started_from_cell(cell: row_cell(row_cells: row_cells, column: 1)) }
row_1_cells: LIST { make_cell(column: 1, row: 1) }
document: Document/new(root: Element/stripe(element: [], direction: Row, gap: 0, style: [], items: make_row_elements(row_cells: row_1_cells)))
edit_started: edit_started_in_row(row_cells: row_1_cells)
"#;

        let ast = parse_source(source).expect("parse");
        let program = lower(&ast, None).expect("lower");

        let edit_started = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| (cell.name == "edit_started").then_some(CellId(idx as u32)))
            .expect("edit_started cell");

        let then_trigger = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::Then { cell, trigger, .. } if *cell == edit_started => Some(*trigger),
                IrNode::Derived {
                    cell,
                    expr: IrExpr::CellRead(source),
                } if *cell == edit_started => program.nodes.iter().find_map(|inner| match inner {
                    IrNode::Then { cell, trigger, .. } if *cell == *source => Some(*trigger),
                    _ => None,
                }),
                _ => None,
            })
            .expect("edit_started then trigger");

        assert_eq!(
            &program.events[then_trigger.0 as usize].name,
            "object.cell_elements.display.double_click"
        );
    }

    #[test]
    fn minimal_multi_column_row_cells_latest_preserves_all_double_click_triggers() {
        let source = r#"
FUNCTION make_cell(column, row) { [row: row, column: column, cell_elements: [display: LINK, editing: LINK]] }
FUNCTION row_cell(row_cells, column) { row_cells |> List/get(index: column) }
FUNCTION edit_started_from_cell(cell) { cell.cell_elements.display.event.double_click |> THEN { [row: cell.row, column: cell.column] } }
FUNCTION edit_started_in_row(row_cells) { LATEST {
    edit_started_from_cell(cell: row_cell(row_cells: row_cells, column: 1))
    edit_started_from_cell(cell: row_cell(row_cells: row_cells, column: 2))
    edit_started_from_cell(cell: row_cell(row_cells: row_cells, column: 3))
} }
row_1_cells: LIST {
    make_cell(column: 1, row: 1)
    make_cell(column: 2, row: 1)
    make_cell(column: 3, row: 1)
}
edit_started: edit_started_in_row(row_cells: row_1_cells)
"#;

        let ast = parse_source(source).expect("parse");
        let program = lower(&ast, None).expect("lower");

        let edit_started = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| (cell.name == "edit_started").then_some(CellId(idx as u32)))
            .expect("edit_started cell");

        let arms = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::Latest { target, arms } if *target == edit_started => Some(arms),
                IrNode::Derived {
                    cell,
                    expr: IrExpr::CellRead(source),
                } if *cell == edit_started => program.nodes.iter().find_map(|inner| match inner {
                    IrNode::Latest { target, arms } if *target == *source => Some(arms),
                    _ => None,
                }),
                _ => None,
            })
            .expect("edit_started latest");

        assert_eq!(arms.len(), 3);
        for arm in arms {
            let trigger = arm.trigger.expect("edit_started arm trigger");
            assert_eq!(
                &program.events[trigger.0 as usize].name,
                "object.cell_elements.display.double_click"
            );
        }
    }

    #[test]
    fn minimal_multi_column_row_data_object_latest_preserves_all_double_click_triggers() {
        let source = r#"
FUNCTION make_cell(column, row) { [row: row, column: column, cell_elements: [display: LINK, editing: LINK]] }
FUNCTION make_row_data(row_number, row_cells) { [row: row_number, cells: row_cells] }
FUNCTION row_cell(row_cells, column) { row_cells |> List/get(index: column) }
FUNCTION edit_started_from_cell(cell) { cell.cell_elements.display.event.double_click |> THEN { [row: cell.row, column: cell.column] } }
FUNCTION edit_started_in_row(row_cells) { LATEST {
    edit_started_from_cell(cell: row_cell(row_cells: row_cells, column: 1))
    edit_started_from_cell(cell: row_cell(row_cells: row_cells, column: 2))
    edit_started_from_cell(cell: row_cell(row_cells: row_cells, column: 3))
} }
row_1_cells: LIST {
    make_cell(column: 1, row: 1)
    make_cell(column: 2, row: 1)
    make_cell(column: 3, row: 1)
}
row_data: make_row_data(row_number: 1, row_cells: row_1_cells)
edit_started: edit_started_in_row(row_cells: row_data.cells)
"#;

        let ast = parse_source(source).expect("parse");
        let program = lower(&ast, None).expect("lower");

        let edit_started = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| (cell.name == "edit_started").then_some(CellId(idx as u32)))
            .expect("edit_started cell");

        let arms = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::Latest { target, arms } if *target == edit_started => Some(arms),
                IrNode::Derived {
                    cell,
                    expr: IrExpr::CellRead(source),
                } if *cell == edit_started => program.nodes.iter().find_map(|inner| match inner {
                    IrNode::Latest { target, arms } if *target == *source => Some(arms),
                    _ => None,
                }),
                _ => None,
            })
            .expect("edit_started latest");

        assert_eq!(arms.len(), 3);
        for arm in arms {
            let trigger = arm.trigger.expect("edit_started arm trigger");
            assert_eq!(
                &program.events[trigger.0 as usize].name,
                "object.cell_elements.display.double_click"
            );
        }
    }

    #[test]
    fn minimal_row_data_cells_projection_tracks_row_cells_source() {
        let source = r#"
FUNCTION make_cell(column, row) { [row: row, column: column, cell_elements: [display: LINK, editing: LINK]] }
FUNCTION make_row_data(row_number, row_cells) { [row: row_number, cells: row_cells] }
row_1_cells: LIST {
    make_cell(column: 1, row: 1)
    make_cell(column: 2, row: 1)
    make_cell(column: 3, row: 1)
}
all_row_cells: LIST { make_row_data(row_number: 1, row_cells: row_1_cells) }
projected: all_row_cells |> List/map(row_data, new: row_data.cells) |> List/latest()
"#;

        let ast = parse_source(source).expect("parse");
        let ctx = super::lower_to_ctx(&ast, None);
        let all_row_cells = ctx
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| (cell.name == "all_row_cells").then_some(CellId(idx as u32)))
            .expect("all_row_cells cell");
        if let Some(representative_fields) = ctx.find_list_item_field_exprs(all_row_cells) {
            let cells_expr = representative_fields
                .get("cells")
                .cloned()
                .unwrap_or(IrExpr::Constant(IrValue::Void));
            let reduced_cells = ctx.reduce_representative_expr(&cells_expr);
            if !matches!(reduced_cells, IrExpr::CellRead(cell) if ctx.cells[cell.0 as usize].name == "row_1_cells")
            {
                panic!(
                    "all_row_cells representative cells field expected row_1_cells, got raw={:?} reduced={:?}",
                    cells_expr, reduced_cells
                );
            }
        } else if let Some(fields) = ctx.resolve_cell_field_cells(all_row_cells) {
            let cells_field = fields
                .get("cells")
                .copied()
                .expect("all_row_cells.cells field");
            let canonical_cells = ctx.canonicalize_shape_source_cell(cells_field);
            let canonical_name = &ctx.cells[canonical_cells.0 as usize].name;
            let is_concrete_list_alias = ctx.find_list_constructor(cells_field).is_some()
                || ctx.find_list_item_field_exprs(cells_field).is_some()
                || ctx.resolve_cell_field_cells(cells_field).is_some();
            if canonical_name != "row_1_cells" && !is_concrete_list_alias {
                panic!(
                    "all_row_cells materialized cells field expected row_1_cells or concrete list alias, got {}",
                    canonical_name
                );
            }
        } else {
            let all_row_cells_node = ctx
                .nodes
                .iter()
                .rev()
                .find(|node| match node {
                    IrNode::Derived { cell, .. }
                    | IrNode::PipeThrough { cell, .. }
                    | IrNode::ListMap { cell, .. }
                    | IrNode::Latest { target: cell, .. }
                    | IrNode::Then { cell, .. }
                    | IrNode::Hold { cell, .. }
                    | IrNode::When { cell, .. }
                    | IrNode::While { cell, .. }
                    | IrNode::ListAppend { cell, .. }
                    | IrNode::ListClear { cell, .. }
                    | IrNode::ListRemove { cell, .. }
                    | IrNode::ListRetain { cell, .. } => *cell == all_row_cells,
                    _ => false,
                })
                .map(|node| format!("{node:?}"))
                .unwrap_or_else(|| "<none>".to_string());
            let first_item_cell = ctx.nodes.iter().rev().find_map(|node| match node {
                IrNode::Derived {
                    cell,
                    expr: IrExpr::ListConstruct(items),
                } if *cell == all_row_cells => items.first().and_then(|item| match item {
                    IrExpr::CellRead(item_cell) => Some(*item_cell),
                    _ => None,
                }),
                _ => None,
            });
            let first_item_node = first_item_cell
                .and_then(|item_cell| {
                    ctx.nodes.iter().rev().find(|node| match node {
                        IrNode::Derived { cell, .. }
                        | IrNode::PipeThrough { cell, .. }
                        | IrNode::ListMap { cell, .. }
                        | IrNode::Latest { target: cell, .. }
                        | IrNode::Then { cell, .. }
                        | IrNode::Hold { cell, .. }
                        | IrNode::When { cell, .. }
                        | IrNode::While { cell, .. }
                        | IrNode::ListAppend { cell, .. }
                        | IrNode::ListClear { cell, .. }
                        | IrNode::ListRemove { cell, .. }
                        | IrNode::ListRetain { cell, .. } => *cell == item_cell,
                        _ => false,
                    })
                })
                .map(|node| format!("{node:?}"))
                .unwrap_or_else(|| "<none>".to_string());
            let first_item_fields = first_item_cell
                .and_then(|item_cell| ctx.cell_field_cells.get(&item_cell).cloned())
                .map(|fields| {
                    let mut entries = fields
                        .into_iter()
                        .map(|(name, cell)| {
                            format!("{}:{}:{}", name, cell.0, ctx.cells[cell.0 as usize].name)
                        })
                        .collect::<Vec<_>>();
                    entries.sort();
                    entries.join(",")
                })
                .unwrap_or_else(|| "<none>".to_string());
            panic!(
                "all_row_cells missing representative item fields; node={} first_item_node={} first_item_fields=[{}]",
                all_row_cells_node, first_item_node, first_item_fields
            );
        }
        let program = lower(&ast, None).expect("lower");

        let projected = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| (cell.name == "projected").then_some(CellId(idx as u32)))
            .expect("projected cell");

        let arm_body = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::Latest { target, arms } if *target == projected => {
                    Some(arms[0].body.clone())
                }
                _ => None,
            })
            .expect("projected latest");

        let projected_source = match arm_body {
            IrExpr::CellRead(cell) => cell,
            IrExpr::FieldAccess { object, field } => {
                let object_cell = match object.as_ref() {
                    IrExpr::CellRead(cell) => *cell,
                    other => panic!(
                        "expected projected field access object to read a cell, got {other:?}"
                    ),
                };
                let object_fields = program
                    .cell_field_cells
                    .get(&object_cell)
                    .cloned()
                    .unwrap_or_default();
                panic!(
                    "projected latest arm stayed FieldAccess; object={} fields={:?}",
                    program.cells[object_cell.0 as usize].name,
                    object_fields
                        .iter()
                        .map(|(name, cell)| format!(
                            "{name}:{}",
                            program.cells[cell.0 as usize].name
                        ))
                        .collect::<Vec<_>>()
                );
            }
            other => panic!("expected projected latest arm to read a cell, got {other:?}"),
        };

        let projected_node = program
            .nodes
            .iter()
            .find(|node| match node {
                IrNode::Derived { cell, .. } | IrNode::PipeThrough { cell, .. } => {
                    *cell == projected_source
                }
                _ => false,
            })
            .expect("projected source node");

        match projected_node {
            IrNode::Derived {
                expr: IrExpr::CellRead(source),
                ..
            }
            | IrNode::PipeThrough { source, .. } => {
                if program.cells[source.0 as usize].name != "row_1_cells" {
                    let source_node = program
                        .nodes
                        .iter()
                        .find(|node| match node {
                            IrNode::Derived { cell, .. }
                            | IrNode::PipeThrough { cell, .. }
                            | IrNode::ListMap { cell, .. }
                            | IrNode::Latest { target: cell, .. }
                            | IrNode::Then { cell, .. }
                            | IrNode::Hold { cell, .. }
                            | IrNode::When { cell, .. }
                            | IrNode::While { cell, .. } => *cell == *source,
                            _ => false,
                        })
                        .map(|node| format!("{node:?}"))
                        .unwrap_or_else(|| "<none>".to_string());
                    let source_object = program
                        .nodes
                        .iter()
                        .find_map(|node| match node {
                            IrNode::Derived {
                                cell,
                                expr: IrExpr::FieldAccess { object, .. },
                            } if *cell == *source => match object.as_ref() {
                                IrExpr::CellRead(object_cell) => Some(format!(
                                    "{}:{}",
                                    object_cell.0, program.cells[object_cell.0 as usize].name
                                )),
                                other => Some(format!("{other:?}")),
                            },
                            _ => None,
                        })
                        .unwrap_or_else(|| "<none>".to_string());
                    let named_nodes = [
                        "row_data",
                        "row_data.cells",
                        "row_cells",
                        "object.cells",
                        "row_1_cells",
                    ]
                    .into_iter()
                    .filter_map(|name| {
                        program
                            .cells
                            .iter()
                            .enumerate()
                            .find_map(|(idx, cell)| {
                                (cell.name == name).then_some((name, CellId(idx as u32)))
                            })
                            .map(|(name, cell_id)| {
                                let node = program
                                    .nodes
                                    .iter()
                                    .find(|node| match node {
                                        IrNode::Derived { cell, .. }
                                        | IrNode::PipeThrough { cell, .. }
                                        | IrNode::ListMap { cell, .. }
                                        | IrNode::Latest { target: cell, .. }
                                        | IrNode::Then { cell, .. }
                                        | IrNode::Hold { cell, .. }
                                        | IrNode::When { cell, .. }
                                        | IrNode::While { cell, .. } => *cell == cell_id,
                                        _ => false,
                                    })
                                    .map(|node| format!("{node:?}"))
                                    .unwrap_or_else(|| "<none>".to_string());
                                format!("{name}={node}")
                            })
                    })
                    .collect::<Vec<_>>()
                    .join(" | ");
                    let source_object_fields = program
                        .cells
                        .get(
                            source_object
                                .split(':')
                                .next()
                                .and_then(|id| id.parse::<usize>().ok())
                                .unwrap_or_default(),
                        )
                        .and_then(|_| {
                            source_object
                                .split(':')
                                .next()
                                .and_then(|id| id.parse::<u32>().ok())
                                .map(CellId)
                        })
                        .and_then(|object_cell| program.cell_field_cells.get(&object_cell).cloned())
                        .map(|fields| {
                            let mut entries = fields
                                .into_iter()
                                .map(|(name, cell)| {
                                    format!(
                                        "{}:{}:{}",
                                        name, cell.0, program.cells[cell.0 as usize].name
                                    )
                                })
                                .collect::<Vec<_>>();
                            entries.sort();
                            entries.join(",")
                        })
                        .unwrap_or_else(|| "<none>".to_string());
                    let source_object_cells_field_node = source_object
                        .split(':')
                        .next()
                        .and_then(|id| id.parse::<u32>().ok())
                        .map(CellId)
                        .and_then(|object_cell| {
                            program
                                .cell_field_cells
                                .get(&object_cell)
                                .and_then(|fields| fields.get("cells"))
                                .copied()
                        })
                        .and_then(|field_cell| {
                            program.nodes.iter().find(|node| match node {
                                IrNode::Derived { cell, .. }
                                | IrNode::PipeThrough { cell, .. }
                                | IrNode::ListMap { cell, .. }
                                | IrNode::Latest { target: cell, .. }
                                | IrNode::Then { cell, .. }
                                | IrNode::Hold { cell, .. }
                                | IrNode::When { cell, .. }
                                | IrNode::While { cell, .. }
                                | IrNode::ListAppend { cell, .. }
                                | IrNode::ListClear { cell, .. }
                                | IrNode::ListRemove { cell, .. }
                                | IrNode::ListRetain { cell, .. } => *cell == field_cell,
                                _ => false,
                            })
                        })
                        .map(|node| format!("{node:?}"))
                        .unwrap_or_else(|| "<none>".to_string());
                    panic!(
                        "projected source expected row_1_cells, got {} node={} source_object={} source_object_fields=[{}] source_object_cells_field_node={} extra=[{}]",
                        program.cells[source.0 as usize].name,
                        source_node,
                        source_object,
                        source_object_fields,
                        source_object_cells_field_node,
                        named_nodes
                    );
                }
            }
            other => panic!("unexpected projected source node: {other:?}"),
        }
    }

    #[test]
    fn minimal_row_data_list_map_item_cells_field_tracks_row_1_cells() {
        let source = r#"
FUNCTION make_cell(column, row) { [row: row, column: column, cell_elements: [display: LINK, editing: LINK]] }
FUNCTION make_row_data(row_number, row_cells) { [row: row_number, cells: row_cells] }
row_1_cells: LIST {
    make_cell(column: 1, row: 1)
    make_cell(column: 2, row: 1)
    make_cell(column: 3, row: 1)
}
all_row_cells: LIST { make_row_data(row_number: 1, row_cells: row_1_cells) }
projected: all_row_cells |> List/map(row_data, new: row_data.cells) |> List/latest()
"#;

        let ast = parse_source(source).expect("parse");
        let ctx = super::lower_to_ctx(&ast, None);
        let all_row_cells = ctx
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| (cell.name == "all_row_cells").then_some(CellId(idx as u32)))
            .expect("all_row_cells cell");
        assert_eq!(
            ctx.find_list_constructor(all_row_cells).as_deref(),
            Some("make_row_data"),
            "all_row_cells constructor lost before template inlining"
        );
        let program = lower(&ast, None).expect("lower");

        let (item_cell, fields) = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::ListMap {
                    source, item_cell, ..
                } if program.cells[source.0 as usize].name == "all_row_cells" => program
                    .cell_field_cells
                    .get(item_cell)
                    .cloned()
                    .map(|fields| (*item_cell, fields)),
                _ => None,
            })
            .expect("all_row_cells list map");

        let cells_field = fields.get("cells").copied().expect("row_data.cells field");
        let source_cell = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::Derived {
                    cell,
                    expr: IrExpr::CellRead(source),
                } if *cell == cells_field => Some(*source),
                _ => None,
            })
            .expect("row_data.cells source");
        let source_node = program.nodes.iter().find(|node| match node {
            IrNode::Derived { cell, .. }
            | IrNode::PipeThrough { cell, .. }
            | IrNode::ListMap { cell, .. }
            | IrNode::Latest { target: cell, .. }
            | IrNode::Then { cell, .. }
            | IrNode::Hold { cell, .. }
            | IrNode::When { cell, .. }
            | IrNode::While { cell, .. } => *cell == source_cell,
            _ => false,
        });
        let source_upstream = program.nodes.iter().find_map(|node| match node {
            IrNode::Derived {
                cell,
                expr: IrExpr::CellRead(source),
            } if *cell == source_cell => Some(*source),
            IrNode::PipeThrough { cell, source } if *cell == source_cell => Some(*source),
            _ => None,
        });
        let source_upstream_node = source_upstream.and_then(|upstream| {
            program.nodes.iter().find(|node| match node {
                IrNode::Derived { cell, .. }
                | IrNode::PipeThrough { cell, .. }
                | IrNode::ListMap { cell, .. }
                | IrNode::Latest { target: cell, .. }
                | IrNode::Then { cell, .. }
                | IrNode::Hold { cell, .. }
                | IrNode::When { cell, .. }
                | IrNode::While { cell, .. } => *cell == upstream,
                _ => false,
            })
        });
        let source_upstream_fields = source_upstream
            .and_then(|upstream| program.cell_field_cells.get(&upstream).cloned())
            .unwrap_or_default();

        assert_eq!(
            program.cells[source_cell.0 as usize].name,
            "row_1_cells",
            "item_cell={} cells_field={} source={} node={:?} source_node={:?} source_upstream={} source_upstream_node={:?} source_upstream_fields={:?}",
            item_cell.0,
            cells_field.0,
            source_cell.0,
            program.nodes.iter().find(|node| match node {
                IrNode::Derived { cell, .. } if *cell == cells_field => true,
                _ => false,
            }),
            source_node,
            source_upstream
                .map(|cell| format!("{}:{}", cell.0, program.cells[cell.0 as usize].name))
                .unwrap_or_else(|| "<none>".to_string()),
            source_upstream_node,
            source_upstream_fields
                .iter()
                .map(|(name, cell)| format!("{name}:{}", program.cells[cell.0 as usize].name))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn minimal_row_data_list_map_item_row_field_tracks_row_number() {
        let source = r#"
FUNCTION make_cell(column, row) { [row: row, column: column, cell_elements: [display: LINK, editing: LINK]] }
FUNCTION make_row_data(row_number, row_cells) { [row: row_number, cells: row_cells] }
row_1_cells: LIST {
    make_cell(column: 1, row: 1)
    make_cell(column: 2, row: 1)
    make_cell(column: 3, row: 1)
}
all_row_cells: LIST { make_row_data(row_number: 1, row_cells: row_1_cells) }
projected: all_row_cells |> List/map(row_data, new: row_data.row) |> List/latest()
"#;

        let ast = parse_source(source).expect("parse");
        let program = lower(&ast, None).expect("lower");

        let (item_cell, fields) = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::ListMap {
                    source, item_cell, ..
                } if program.cells[source.0 as usize].name == "all_row_cells" => program
                    .cell_field_cells
                    .get(item_cell)
                    .cloned()
                    .map(|fields| (*item_cell, fields)),
                _ => None,
            })
            .expect("all_row_cells list map");

        let row_field = fields.get("row").copied().expect("row_data.row field");
        let row_field_node = program.nodes.iter().find(|node| match node {
            IrNode::Derived { cell, .. }
            | IrNode::PipeThrough { cell, .. }
            | IrNode::ListMap { cell, .. }
            | IrNode::Latest { target: cell, .. }
            | IrNode::Then { cell, .. }
            | IrNode::Hold { cell, .. }
            | IrNode::When { cell, .. }
            | IrNode::While { cell, .. } => *cell == row_field,
            _ => false,
        });
        let source_cell = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::Derived {
                    cell,
                    expr: IrExpr::CellRead(source),
                } if *cell == row_field => Some(*source),
                _ => None,
            })
            .unwrap_or_else(|| panic!("row_data.row source; node={row_field_node:?}"));

        assert_eq!(
            program.cells[source_cell.0 as usize].name,
            "1",
            "item_cell={} row_field={} source={} source_name={} source_node={:?}",
            item_cell.0,
            row_field.0,
            source_cell.0,
            program.cells[source_cell.0 as usize].name,
            program.nodes.iter().find(|node| match node {
                IrNode::Derived { cell, .. }
                | IrNode::PipeThrough { cell, .. }
                | IrNode::ListMap { cell, .. }
                | IrNode::Latest { target: cell, .. }
                | IrNode::Then { cell, .. }
                | IrNode::Hold { cell, .. }
                | IrNode::When { cell, .. }
                | IrNode::While { cell, .. } => *cell == source_cell,
                _ => false,
            })
        );
    }

    #[test]
    fn minimal_multi_column_row_data_latest_preserves_all_double_click_triggers() {
        let source = r#"
FUNCTION make_cell(column, row) { [row: row, column: column, cell_elements: [display: LINK, editing: LINK]] }
FUNCTION make_row_data(row_number, row_cells) { [row: row_number, cells: row_cells] }
FUNCTION row_cell(row_cells, column) { row_cells |> List/get(index: column) }
FUNCTION make_cell_element(cell) {
    False |> WHILE {
        True => Element/text_input(element: [event: [change: LINK, key_down: LINK, blur: LINK]] text: Text/empty()) |> LINK { cell.cell_elements.editing }
        False => Element/label(element: [event: [double_click: LINK]], label: TEXT { x }) |> LINK { cell.cell_elements.display }
    }
}
FUNCTION make_row_elements(row_cells) { row_cells |> List/map(cell, new: make_cell_element(cell: cell)) }
FUNCTION edit_started_from_cell(cell) { cell.cell_elements.display.event.double_click |> THEN { [row: cell.row, column: cell.column] } }
FUNCTION edit_started_in_row(row_cells) { LATEST {
    edit_started_from_cell(cell: row_cell(row_cells: row_cells, column: 1))
    edit_started_from_cell(cell: row_cell(row_cells: row_cells, column: 2))
    edit_started_from_cell(cell: row_cell(row_cells: row_cells, column: 3))
} }
row_1_cells: LIST {
    make_cell(column: 1, row: 1)
    make_cell(column: 2, row: 1)
    make_cell(column: 3, row: 1)
}
all_row_cells: LIST { make_row_data(row_number: 1, row_cells: row_1_cells) }
document: Document/new(root: Element/stripe(element: [], direction: Row, gap: 0, style: [], items: make_row_elements(row_cells: row_1_cells)))
edit_started: all_row_cells |> List/map(row_data, new: edit_started_in_row(row_cells: row_data.cells)) |> List/latest()
"#;

        let ast = parse_source(source).expect("parse");
        let program = lower(&ast, None).expect("lower");

        let edit_started = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| (cell.name == "edit_started").then_some(CellId(idx as u32)))
            .expect("edit_started cell");

        let arms = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::Latest { target, arms } if *target == edit_started => Some(arms),
                _ => None,
            })
            .expect("edit_started latest");

        assert_eq!(arms.len(), 3);
        for arm in arms {
            let trigger = arm.trigger.expect("edit_started arm trigger");
            assert_eq!(
                &program.events[trigger.0 as usize].name,
                "object.cell_elements.display.double_click"
            );
        }
    }

    #[test]
    fn minimal_dynamic_row_data_latest_preserves_all_double_click_triggers() {
        let source = r#"
FUNCTION make_cell(column, row) { [row: row, column: column, cell_elements: [display: LINK, editing: LINK]] }
FUNCTION make_row_cells(row) { LIST {
    make_cell(column: 1, row: row)
    make_cell(column: 2, row: row)
    make_cell(column: 3, row: row)
} }
FUNCTION make_row_data(row_number, row_cells) { [row: row_number, cells: row_cells] }
FUNCTION row_cell(row_cells, column) { row_cells |> List/get(index: column) }
FUNCTION edit_started_from_cell(cell) { cell.cell_elements.display.event.double_click |> THEN { [row: cell.row, column: cell.column] } }
FUNCTION edit_started_in_row(row_cells) { row_cells |> List/map(cell, new: edit_started_from_cell(cell: cell)) |> List/latest() }
all_row_cells: List/range(from: 1, to: 2)
    |> List/map(row_number, new:
        make_row_data(
            row_number: row_number
            row_cells: make_row_cells(row: row_number)
        )
    )
edit_started: all_row_cells |> List/map(row_data, new: edit_started_in_row(row_cells: row_data.cells)) |> List/latest()
"#;

        let ast = parse_source(source).expect("parse");
        let program = lower(&ast, None).expect("lower");

        let edit_started = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| (cell.name == "edit_started").then_some(CellId(idx as u32)))
            .expect("edit_started cell");

        let arms = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::Latest { target, arms } if *target == edit_started => Some(arms),
                _ => None,
            })
            .expect("edit_started latest");

        assert_eq!(arms.len(), 2);
        for arm in arms {
            let trigger = arm.trigger.expect("edit_started arm trigger");
            assert_eq!(
                &program.events[trigger.0 as usize].name,
                "object.cell_elements.display.double_click"
            );
        }
    }

    #[test]
    fn minimal_multi_column_row_data_latest_row_cell_preserves_cell_elements_shape() {
        let source = r#"
FUNCTION make_cell(column, row) { [row: row, column: column, cell_elements: [display: LINK, editing: LINK]] }
FUNCTION make_row_data(row_number, row_cells) { [row: row_number, cells: row_cells] }
FUNCTION row_cell(row_cells, column) { row_cells |> List/get(index: column) }
row_1_cells: LIST {
    make_cell(column: 1, row: 1)
    make_cell(column: 2, row: 1)
    make_cell(column: 3, row: 1)
}
all_row_cells: LIST { make_row_data(row_number: 1, row_cells: row_1_cells) }
selected_cell: all_row_cells |> List/map(row_data, new: row_cell(row_cells: row_data.cells, column: 1)) |> List/latest()
"#;

        let ast = parse_source(source).expect("parse");
        let program = lower(&ast, None).expect("lower");

        let selected_cell = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| (cell.name == "selected_cell").then_some(CellId(idx as u32)))
            .expect("selected_cell cell");

        let arm_body = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::Latest { target, arms } if *target == selected_cell => {
                    Some(arms[0].body.clone())
                }
                _ => None,
            })
            .expect("selected_cell latest");

        let selected_source = match arm_body {
            IrExpr::CellRead(cell) => cell,
            other => panic!("expected selected_cell latest arm to read a cell, got {other:?}"),
        };

        let selected_fields = program
            .cell_field_cells
            .get(&selected_source)
            .cloned()
            .unwrap_or_default();
        let metadata_source = {
            let mut current = selected_source;
            let mut seen = std::collections::HashSet::new();
            for _ in 0..20 {
                if !seen.insert(current) {
                    break;
                }
                let next = program.nodes.iter().rev().find_map(|node| match node {
                    IrNode::Derived {
                        cell,
                        expr: IrExpr::CellRead(src),
                    } if *cell == current => Some(*src),
                    IrNode::Derived { cell, expr } if *cell == current => match expr {
                        IrExpr::FieldAccess { object, field } => match object.as_ref() {
                            IrExpr::CellRead(obj_cell) => program
                                .cell_field_cells
                                .get(obj_cell)
                                .and_then(|fields| fields.get(field))
                                .copied(),
                            _ => None,
                        },
                        _ => None,
                    },
                    IrNode::PipeThrough { cell, source } if *cell == current => Some(*source),
                    _ => None,
                });
                match next {
                    Some(next) if next != current => current = next,
                    _ => break,
                }
            }
            current
        };
        let selected_node = program
            .nodes
            .iter()
            .rev()
            .find(|node| match node {
                IrNode::Derived { cell, .. }
                | IrNode::PipeThrough { cell, .. }
                | IrNode::ListMap { cell, .. }
                | IrNode::Latest { target: cell, .. }
                | IrNode::Then { cell, .. }
                | IrNode::Hold { cell, .. }
                | IrNode::When { cell, .. }
                | IrNode::While { cell, .. } => *cell == selected_source,
                _ => false,
            })
            .map(|node| format!("{node:?}"))
            .unwrap_or_else(|| "<none>".to_string());
        let selected_source_upstream = program
            .nodes
            .iter()
            .rev()
            .find_map(|node| match node {
                IrNode::Derived {
                    cell,
                    expr: IrExpr::CellRead(src),
                } if *cell == selected_source => Some(*src),
                IrNode::PipeThrough { cell, source } if *cell == selected_source => Some(*source),
                _ => None,
            })
            .map(|cell| {
                let upstream_name = program.cells[cell.0 as usize].name.clone();
                let upstream_fields = program
                    .cell_field_cells
                    .get(&cell)
                    .cloned()
                    .unwrap_or_default();
                let upstream_object = program
                    .nodes
                    .iter()
                    .rev()
                    .find_map(|node| match node {
                        IrNode::Derived {
                            cell: c,
                            expr: IrExpr::FieldAccess { object, field },
                        } if *c == cell => match object.as_ref() {
                            IrExpr::CellRead(obj_cell) => Some(format!(
                                "{} field={} object_fields={:?}",
                                program.cells[obj_cell.0 as usize].name,
                                field,
                                program
                                    .cell_field_cells
                                    .get(obj_cell)
                                    .cloned()
                                    .unwrap_or_default()
                                    .iter()
                                    .map(|(name, cell)| format!(
                                        "{name}:{}",
                                        program.cells[cell.0 as usize].name
                                    ))
                                    .collect::<Vec<_>>()
                            )),
                            other => Some(format!("non-cell object {other:?}")),
                        },
                        _ => None,
                    })
                    .unwrap_or_else(|| "<none>".to_string());
                let upstream_node = program
                    .nodes
                    .iter()
                    .rev()
                    .find(|node| match node {
                        IrNode::Derived { cell: c, .. }
                        | IrNode::PipeThrough { cell: c, .. }
                        | IrNode::ListMap { cell: c, .. }
                        | IrNode::Latest { target: c, .. }
                        | IrNode::Then { cell: c, .. }
                        | IrNode::Hold { cell: c, .. }
                        | IrNode::When { cell: c, .. }
                        | IrNode::While { cell: c, .. } => *c == cell,
                        _ => false,
                    })
                    .map(|node| format!("{node:?}"))
                    .unwrap_or_else(|| "<none>".to_string());
                format!(
                    "{} fields={:?} object={} node={}",
                    upstream_name,
                    upstream_fields
                        .iter()
                        .map(|(name, cell)| format!(
                            "{name}:{}",
                            program.cells[cell.0 as usize].name
                        ))
                        .collect::<Vec<_>>(),
                    upstream_object,
                    upstream_node
                )
            })
            .unwrap_or_else(|| "<none>".to_string());
        assert!(
            selected_fields.contains_key("cell_elements"),
            "selected_cell source lost cell_elements: cell={} metadata_source={} upstream={} node={} fields={:?}",
            program.cells[selected_source.0 as usize].name,
            program.cells[metadata_source.0 as usize].name,
            selected_source_upstream,
            selected_node,
            selected_fields
                .iter()
                .map(|(name, cell)| format!("{name}:{}", program.cells[cell.0 as usize].name))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn crud_render_list_map_template_contains_person_id_leaf() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/crud/crud.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");

        let render_map = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::ListMap {
                    item_cell,
                    template_cell_range,
                    ..
                } => {
                    let fields = program.cell_field_cells.get(item_cell)?;
                    let surname = fields.get("surname")?;
                    let name = fields.get("name")?;
                    let id = fields.get("id")?;
                    Some((*item_cell, *surname, *name, *id, *template_cell_range))
                }
                _ => None,
            })
            .expect("render list map");

        let (_item_cell, _surname_cell, _name_cell, id_cell, (start, end)) = render_map;
        let id_leaf = match program.nodes.iter().find(|node| match node {
            IrNode::Derived {
                cell,
                expr: IrExpr::TaggedObject { .. },
            } => *cell == id_cell,
            _ => false,
        }) {
            Some(IrNode::Derived {
                expr: IrExpr::TaggedObject { fields, .. },
                ..
            }) => fields
                .iter()
                .find_map(|(field, expr)| {
                    (field == "id").then(|| match expr {
                        IrExpr::CellRead(cell) => *cell,
                        _ => CellId(u32::MAX),
                    })
                })
                .expect("PersonId.id leaf"),
            _ => id_cell,
        };

        assert!(
            id_leaf.0 >= start && id_leaf.0 < end,
            "item.id leaf {} should be inside template range {:?}",
            program.cells[id_leaf.0 as usize].name,
            (start, end)
        );
    }

    #[test]
    fn todo_mvc_render_list_map_item_preserves_title_and_completed_fields() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");

        let render_map_fields = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::ListMap {
                    item_cell,
                    template_cell_range,
                    ..
                } => program.cell_field_cells.get(item_cell).and_then(|fields| {
                    (fields.contains_key("title") && fields.contains_key("completed"))
                        .then_some((fields.clone(), *template_cell_range))
                }),
                _ => None,
            })
            .expect("todo_mvc render list map field map");

        let (fields, (start, end)) = render_map_fields;
        let title_cell = fields.get("title").copied().expect("title field");
        let completed_cell = fields.get("completed").copied().expect("completed field");

        assert!(
            title_cell.0 >= start && title_cell.0 < end,
            "title field {} should be inside template range {:?}",
            program.cells[title_cell.0 as usize].name,
            (start, end)
        );
        assert!(
            completed_cell.0 >= start && completed_cell.0 < end,
            "completed field {} should be inside template range {:?}",
            program.cells[completed_cell.0 as usize].name,
            (start, end)
        );
    }

    #[test]
    #[ignore = "temporary lowering inspection"]
    fn inspect_todo_mvc_remove_button_wiring() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");

        for (idx, event) in program.events.iter().enumerate() {
            if event.name.contains("remove_todo_button") || event.name.contains("press") {
                println!(
                    "event[{idx}] {} payload={:?}",
                    event.name, event.payload_cells
                );
            }
        }

        for node in &program.nodes {
            if let IrNode::ListRemove {
                cell,
                source,
                trigger,
                predicate,
                item_cell,
                item_field_cells,
            } = node
            {
                println!(
                    "ListRemove cell={}({}) source={}({}) trigger={} pred={:?} item={:?} fields={:?}",
                    cell.0,
                    program.cells[cell.0 as usize].name,
                    source.0,
                    program.cells[source.0 as usize].name,
                    trigger.0,
                    predicate.map(|cell| cell.0),
                    item_cell.map(|cell| (cell.0, program.cells[cell.0 as usize].name.clone())),
                    item_field_cells
                        .iter()
                        .map(|(name, cell)| format!(
                            "{name}:{}({})",
                            cell.0, program.cells[cell.0 as usize].name
                        ))
                        .collect::<Vec<_>>()
                );
            }
        }

        for target in [CellId(6), CellId(49), CellId(50)] {
            println!(
                "--- trace for cell {} ({}) ---",
                target.0, program.cells[target.0 as usize].name
            );
            for node in &program.nodes {
                match node {
                    IrNode::PipeThrough { cell, source }
                        if *cell == target || *source == target =>
                    {
                        println!(
                            "  {}",
                            crate::platform::browser::engine_wasm::ir::node_debug_short(node)
                        );
                    }
                    IrNode::ListAppend { cell, source, .. }
                        if *cell == target || *source == target =>
                    {
                        println!(
                            "  {}",
                            crate::platform::browser::engine_wasm::ir::node_debug_short(node)
                        );
                    }
                    IrNode::Derived { cell, expr } if *cell == target => {
                        println!("  Derived cell={} expr={expr:?}", cell.0);
                    }
                    IrNode::ListMap { cell, source, .. }
                        if *cell == target || *source == target =>
                    {
                        println!(
                            "  {}",
                            crate::platform::browser::engine_wasm::ir::node_debug_short(node)
                        );
                    }
                    IrNode::ListRemove { cell, source, .. }
                        if *cell == target || *source == target =>
                    {
                        println!(
                            "  {}",
                            crate::platform::browser::engine_wasm::ir::node_debug_short(node)
                        );
                    }
                    IrNode::ListRetain {
                        cell,
                        source,
                        predicate,
                        ..
                    } if *cell == target || *source == target || *predicate == Some(target) => {
                        println!(
                            "  {}",
                            crate::platform::browser::engine_wasm::ir::node_debug_short(node)
                        );
                    }
                    IrNode::Hold { cell, init, .. } if *cell == target => {
                        println!("  Hold cell={} init={init:?}", cell.0);
                    }
                    _ => {}
                }
            }
        }
    }

    #[test]
    #[ignore = "temporary lowering inspection"]
    fn crud_selected_id_map_item_id_matches_render_map_item_id_shape() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/crud/crud.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");

        for node in &program.nodes {
            let IrNode::ListMap {
                cell,
                item_cell,
                template_cell_range,
                ..
            } = node
            else {
                continue;
            };

            let Some(fields) = program.cell_field_cells.get(item_cell) else {
                continue;
            };
            let Some(id_cell) = fields.get("id").copied() else {
                continue;
            };

            let id_leaf = match program.nodes.iter().find(|node| match node {
                IrNode::Derived {
                    cell,
                    expr: IrExpr::TaggedObject { .. },
                } => *cell == id_cell,
                _ => false,
            }) {
                Some(IrNode::Derived {
                    expr: IrExpr::TaggedObject { fields, .. },
                    ..
                }) => fields
                    .iter()
                    .find_map(|(field, expr)| {
                        (field == "id").then(|| match expr {
                            IrExpr::CellRead(cell) => *cell,
                            _ => CellId(u32::MAX),
                        })
                    })
                    .expect("PersonId.id leaf"),
                _ => id_cell,
            };

            println!(
                "list_map cell={} range={:?} item.id={} leaf={} names=({}, {})",
                program.cells[cell.0 as usize].name,
                template_cell_range,
                program.cells[id_cell.0 as usize].name,
                program.cells[id_leaf.0 as usize].name,
                id_cell.0,
                id_leaf.0
            );
        }
        panic!("inspect list_map item.id lowering");
    }

    #[test]
    fn temperature_converter_text_inputs_keep_nested_while_dependencies() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/temperature_converter/temperature_converter.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");

        let celsius_raw = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| {
                (cell.name == "store.celsius_raw").then_some(CellId(idx as u32))
            })
            .expect("store.celsius_raw");
        let fahrenheit_raw = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| {
                (cell.name == "store.fahrenheit_raw").then_some(CellId(idx as u32))
            })
            .expect("store.fahrenheit_raw");
        let last_edited = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| {
                (cell.name == "store.last_edited").then_some(CellId(idx as u32))
            })
            .expect("store.last_edited");

        let mut saw_celsius_dependency = false;
        let mut saw_fahrenheit_dependency = false;

        let resolve_text_cell = |mut cell: CellId| {
            loop {
                let next = program.nodes.iter().find_map(|node| match node {
                    IrNode::Derived {
                        cell: candidate,
                        expr: IrExpr::CellRead(source),
                    } if *candidate == cell => Some(*source),
                    IrNode::PipeThrough {
                        cell: candidate,
                        source,
                    } if *candidate == cell => Some(*source),
                    _ => None,
                });
                let Some(next) = next else {
                    return cell;
                };
                cell = next;
            }
        };

        for node in &program.nodes {
            let IrNode::Element {
                kind:
                    ElementKind::TextInput {
                        text_cell: Some(text_cell),
                        ..
                    },
                ..
            } = node
            else {
                continue;
            };

            let text_cell = resolve_text_cell(*text_cell);
            let Some(IrNode::While { source, deps, .. }) = program.nodes.iter().find(
                |candidate| matches!(candidate, IrNode::While { cell, .. } if *cell == text_cell),
            ) else {
                continue;
            };

            assert_eq!(
                *source, last_edited,
                "text_input.text WHILE should match on store.last_edited"
            );

            if deps.contains(&celsius_raw) {
                saw_celsius_dependency = true;
            }
            if deps.contains(&fahrenheit_raw) {
                saw_fahrenheit_dependency = true;
            }
        }

        assert!(
            saw_celsius_dependency,
            "expected one text input to depend on store.celsius_raw"
        );
        assert!(
            saw_fahrenheit_dependency,
            "expected one text input to depend on store.fahrenheit_raw"
        );
    }

    #[test]
    fn interval_hold_counter_lowers_to_stream_skip() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/interval_hold/interval_hold.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");

        let counter = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| (cell.name == "counter").then_some(CellId(idx as u32)))
            .expect("counter cell");

        let Some(IrNode::StreamSkip {
            source,
            count,
            seen_cell: _,
            ..
        }) = program
            .nodes
            .iter()
            .find(|node| matches!(node, IrNode::StreamSkip { cell, .. } if *cell == counter))
        else {
            panic!("counter should lower to StreamSkip, got no StreamSkip node");
        };

        assert_eq!(
            *count, 1,
            "interval_hold should skip exactly one initial value"
        );
        assert_ne!(
            *source, counter,
            "StreamSkip should wrap the HOLD source rather than self-reference"
        );
        assert!(
            program
                .nodes
                .iter()
                .any(|node| matches!(node, IrNode::Hold { cell, init: IrExpr::Constant(IrValue::Number(0.0)), .. } if *cell == *source)),
            "StreamSkip source should be the HOLD cell initialized from literal 0"
        );
        let hold_trigger_bodies = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::Hold {
                    cell,
                    trigger_bodies,
                    ..
                } if *cell == *source => Some(trigger_bodies),
                _ => None,
            })
            .expect("interval_hold HOLD node");
        assert_eq!(
            hold_trigger_bodies.len(),
            1,
            "interval_hold HOLD should have exactly one timer-triggered body"
        );
        assert!(
            matches!(
                &hold_trigger_bodies[0].1,
                IrExpr::BinOp {
                    op: super::super::ir::BinOp::Add,
                    lhs,
                    rhs,
                }
                if matches!(&**lhs, IrExpr::CellRead(read) if *read == *source)
                    && matches!(&**rhs, IrExpr::Constant(IrValue::Number(n)) if (*n - 1.0).abs() < f64::EPSILON)
            ),
            "interval_hold HOLD body should be `counter + 1` over its own source cell"
        );
    }

    #[test]
    fn fibonacci_result_keeps_current_field_through_stream_skip_and_log_info() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/fibonacci/fibonacci.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");

        let current = program
            .cells
            .iter()
            .enumerate()
            .find_map(|(idx, cell)| (cell.name == "state.current").then_some(CellId(idx as u32)))
            .expect("state.current cell");
        let stream_skip_cell = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::StreamSkip { cell, .. } => Some(*cell),
                _ => None,
            })
            .expect("fibonacci stream skip cell");
        let log_info_cell = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::CustomCall { cell, path, .. }
                    if path == &["Log".to_string(), "info".to_string()] =>
                {
                    Some(*cell)
                }
                _ => None,
            })
            .expect("fibonacci log/info cell");
        let derived_current_cell = program
            .nodes
            .iter()
            .find_map(|node| match node {
                IrNode::Derived { cell, expr }
                    if matches!(expr, IrExpr::CellRead(read) if *read == current) =>
                {
                    Some(*cell)
                }
                _ => None,
            })
            .expect("derived state.current cell");

        assert!(
            matches!(
                program.cell_field_cells.get(&stream_skip_cell).and_then(|fields| fields.get("current")),
                Some(field_cell) if *field_cell == current
            ),
            "Stream/skip should preserve the `current` field mapping for fibonacci state"
        );
        assert!(
            matches!(
                program.cell_field_cells.get(&log_info_cell).and_then(|fields| fields.get("current")),
                Some(field_cell) if *field_cell == current
            ),
            "Log/info should preserve the `current` field mapping for fibonacci state"
        );
        assert!(
            derived_current_cell != current,
            "fibonacci should still derive a downstream cell from state.current after Log/info"
        );
    }

    #[test]
    #[ignore = "temporary lowering inspection"]
    fn inspect_fibonacci_result_path() {
        let ast = parse_source(include_str!(
            "../../../../../../playground/frontend/src/examples/fibonacci/fibonacci.bn"
        ))
        .expect("parse");
        let program = lower(&ast, None).expect("lower");

        for (idx, cell) in program.cells.iter().enumerate() {
            if cell.name == "result"
                || cell.name == "state.previous"
                || cell.name == "state.current"
                || cell.name.contains("pipe_result")
                || cell.name.contains("stream_skip")
            {
                println!("cell[{idx}] {}", cell.name);
            }
        }

        for node in &program.nodes {
            let matches_cell = match node {
                IrNode::Derived { cell, .. }
                | IrNode::Hold { cell, .. }
                | IrNode::Then { cell, .. }
                | IrNode::When { cell, .. }
                | IrNode::While { cell, .. }
                | IrNode::MathSum { cell, .. }
                | IrNode::PipeThrough { cell, .. }
                | IrNode::StreamSkip { cell, .. }
                | IrNode::TextInterpolation { cell, .. }
                | IrNode::CustomCall { cell, .. }
                | IrNode::Element { cell, .. }
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
                | IrNode::HoldLoop { cell, .. } => {
                    matches!(cell.0, 1 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | 11)
                }
                IrNode::Latest { target, .. } => {
                    matches!(target.0, 1 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | 11)
                }
                IrNode::Document { root, .. } => {
                    matches!(root.0, 1 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | 11)
                }
                _ => false,
            };
            if matches_cell {
                println!("{node:?}");
            }
        }

        for (cell, fields) in &program.cell_field_cells {
            let name = &program.cells[cell.0 as usize].name;
            if name == "result"
                || name == "state.previous"
                || name == "state.current"
                || name.contains("pipe_result")
            {
                println!(
                    "fields {} -> {:?}",
                    name,
                    fields
                        .iter()
                        .map(|(field, cell)| format!(
                            "{field}:{}",
                            program.cells[cell.0 as usize].name
                        ))
                        .collect::<Vec<_>>()
                );
            }
        }
    }
}

/// Wrap a value in a Spanned (needed for some recursive lowering).
fn spanned<T>(node: T, span: Span) -> Spanned<T> {
    Spanned {
        span,
        persistence: None,
        node,
    }
}

/// Pre-scan an expression for Element function calls with LINK patterns.
/// Allocates EventIds for each discovered LINK and stores them in ctx.element_events.
fn pre_scan_links_in_expr(
    expr: &Expression,
    var_name: &str,
    element_cell: CellId,
    span: Span,
    ctx: &mut Lowerer,
) {
    match expr {
        Expression::FunctionCall { path, arguments } => {
            let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
            // Scan Element/* and Scene/Element/* calls for LINK patterns.
            let is_element = path_strs.first() == Some(&"Element")
                || (path_strs.len() == 3 && path_strs[0] == "Scene" && path_strs[1] == "Element");
            if is_element {
                if let Some(elem_arg) = arguments.iter().find(|a| a.node.name.as_str() == "element")
                {
                    if let Some(ref val) = elem_arg.node.value {
                        extract_link_events_from_ast(&val.node, var_name, element_cell, span, ctx);
                    }
                }
            }
            // Follow user-defined function calls to find Element definitions inside.
            let resolved = if path_strs.len() == 1 {
                ctx.resolve_func_name(path_strs[0])
            } else if path_strs.len() == 2 {
                let qualified = format!("{}/{}", path_strs[0], path_strs[1]);
                ctx.resolve_func_name(&qualified)
            } else if path_strs.len() == 3 {
                let qualified = format!("{}/{}/{}", path_strs[0], path_strs[1], path_strs[2]);
                ctx.resolve_func_name(&qualified)
            } else {
                None
            };
            if let Some(fn_name) = resolved {
                if let Some(func_def) = ctx.find_func_def(&fn_name) {
                    pre_scan_links_in_expr(&func_def.body.node, var_name, element_cell, span, ctx);
                }
            }
        }
        // Recurse into pipe chains to find element calls.
        Expression::Pipe { from, to } => {
            pre_scan_links_in_expr(&from.node, var_name, element_cell, span, ctx);
            pre_scan_links_in_expr(&to.node, var_name, element_cell, span, ctx);
        }
        _ => {}
    }
}

/// Collect all CellRead references from an IrExpr tree.
fn collect_cell_refs(expr: &IrExpr, out: &mut Vec<CellId>) {
    match expr {
        IrExpr::CellRead(cell) => out.push(*cell),
        IrExpr::BinOp { lhs, rhs, .. } => {
            collect_cell_refs(lhs, out);
            collect_cell_refs(rhs, out);
        }
        IrExpr::UnaryNeg(inner) | IrExpr::Not(inner) => collect_cell_refs(inner, out),
        IrExpr::Compare { lhs, rhs, .. } => {
            collect_cell_refs(lhs, out);
            collect_cell_refs(rhs, out);
        }
        IrExpr::FieldAccess { object, .. } => collect_cell_refs(object, out),
        IrExpr::TextConcat(segs) => {
            for seg in segs {
                if let TextSegment::Expr(e) = seg {
                    collect_cell_refs(e, out);
                }
            }
        }
        IrExpr::FunctionCall { args, .. } => {
            for arg in args {
                collect_cell_refs(arg, out);
            }
        }
        IrExpr::ObjectConstruct(fields) => {
            for (_, val) in fields {
                collect_cell_refs(val, out);
            }
        }
        IrExpr::ListConstruct(items) => {
            for item in items {
                collect_cell_refs(item, out);
            }
        }
        IrExpr::TaggedObject { fields, .. } => {
            for (_, val) in fields {
                collect_cell_refs(val, out);
            }
        }
        IrExpr::Constant(_) => {}
        IrExpr::PatternMatch { source, arms } => {
            out.push(*source);
            for (_, body) in arms {
                collect_cell_refs(body, out);
            }
        }
    }
}

/// Extract LINK event bindings from an element's `element:` argument AST.
/// The pattern is: `element: [event: [press: LINK, change: LINK, ...]]`
fn extract_link_events_from_ast(
    expr: &Expression,
    var_name: &str,
    element_cell: CellId,
    span: Span,
    ctx: &mut Lowerer,
) {
    // The element argument is an Object: [event: [press: LINK]]
    if let Expression::Object(obj) = expr {
        for field in &obj.variables {
            if field.node.name.as_str() == "event" {
                // The event field's value is another Object: [press: LINK]
                if let Expression::Object(event_obj) = &field.node.value.node {
                    for event_field in &event_obj.variables {
                        if matches!(event_field.node.value.node, Expression::Link) {
                            let event_name = event_field.node.name.as_str().to_string();
                            let event_id = ctx.alloc_event(
                                &format!("{}.{}", var_name, event_name),
                                EventSource::Link {
                                    element: element_cell,
                                    event_name: event_name.clone(),
                                },
                                span,
                            );
                            ctx.element_events
                                .entry(var_name.to_string())
                                .or_default()
                                .insert(event_name, event_id);
                        }
                    }
                }
            }
        }
    }
}

/// Pre-scan an AST expression tree for field accesses on `item_name`.
/// Returns a list of unique field names (e.g., ["completed"]).
fn collect_item_field_names(expr: &Spanned<Expression>, item_name: &str) -> Vec<String> {
    let mut fields = Vec::new();
    scan_expr_for_fields(&expr.node, item_name, &mut fields);
    fields
}

fn scan_expr_for_fields(expr: &Expression, item_name: &str, fields: &mut Vec<String>) {
    match expr {
        Expression::Alias(Alias::WithoutPassed { parts, .. }) => {
            if parts.len() >= 2 && parts[0].as_str() == item_name {
                let field = parts[1].as_str().to_string();
                if !fields.contains(&field) {
                    fields.push(field);
                }
            }
        }
        Expression::While { arms } | Expression::When { arms } => {
            for arm in arms {
                scan_expr_for_fields(&arm.body.node, item_name, fields);
            }
        }
        Expression::Pipe { from, to } => {
            scan_expr_for_fields(&from.node, item_name, fields);
            scan_expr_for_fields(&to.node, item_name, fields);
        }
        Expression::FunctionCall { arguments, .. } => {
            for arg in arguments {
                if let Some(val) = &arg.node.value {
                    scan_expr_for_fields(&val.node, item_name, fields);
                }
            }
        }
        Expression::Then { body }
        | Expression::Hold { body, .. }
        | Expression::Flush { value: body }
        | Expression::Spread { value: body } => {
            scan_expr_for_fields(&body.node, item_name, fields);
        }
        Expression::Latest { inputs } => {
            for input in inputs {
                scan_expr_for_fields(&input.node, item_name, fields);
            }
        }
        Expression::Block { variables, output } => {
            for var in variables {
                scan_expr_for_fields(&var.node.value.node, item_name, fields);
            }
            scan_expr_for_fields(&output.node, item_name, fields);
        }
        Expression::Comparator(cmp) => match cmp {
            Comparator::Equal {
                operand_a,
                operand_b,
            }
            | Comparator::NotEqual {
                operand_a,
                operand_b,
            }
            | Comparator::Greater {
                operand_a,
                operand_b,
            }
            | Comparator::GreaterOrEqual {
                operand_a,
                operand_b,
            }
            | Comparator::Less {
                operand_a,
                operand_b,
            }
            | Comparator::LessOrEqual {
                operand_a,
                operand_b,
            } => {
                scan_expr_for_fields(&operand_a.node, item_name, fields);
                scan_expr_for_fields(&operand_b.node, item_name, fields);
            }
        },
        Expression::ArithmeticOperator(op) => match op {
            ArithmeticOperator::Negate { operand } => {
                scan_expr_for_fields(&operand.node, item_name, fields);
            }
            ArithmeticOperator::Add {
                operand_a,
                operand_b,
            }
            | ArithmeticOperator::Subtract {
                operand_a,
                operand_b,
            }
            | ArithmeticOperator::Multiply {
                operand_a,
                operand_b,
            }
            | ArithmeticOperator::Divide {
                operand_a,
                operand_b,
            } => {
                scan_expr_for_fields(&operand_a.node, item_name, fields);
                scan_expr_for_fields(&operand_b.node, item_name, fields);
            }
        },
        Expression::List { items } => {
            for item in items {
                scan_expr_for_fields(&item.node, item_name, fields);
            }
        }
        _ => {}
    }
}
