//! AST → IR lowering for the WASM compilation engine.
//!
//! Two-pass approach:
//! 1. Registration: Walk top-level expressions, collect variable and function definitions.
//! 2. Lowering: Walk each variable's expression tree, emit IrNodes, allocate CellIds/EventIds.

use std::collections::HashMap;

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
pub type ExternalFunction = (
    String,
    Vec<String>,
    Spanned<Expression>,
    Option<String>,
);

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
}

// ---------------------------------------------------------------------------
// Compile errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct CompileError {
    pub span: Span,
    pub message: String,
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

    /// Pending constructor name from the most recent LIST expression lowering.
    /// Consumed by `lower_expr_to_cell` when the ListConstruct gets assigned to a cell.
    pending_list_constructor: Option<String>,

    /// When true, object literals are always lowered as stores (creating cells for each field)
    /// even if the fields aren't reactive. Set during list constructor template inlining
    /// so that field access resolves to actual cells instead of ObjectConstruct.
    force_object_store: bool,

    /// Cells known to hold constant values at compile time.
    /// Used for constant folding of WHEN/WHILE arms during function inlining:
    /// when a cell is known to be e.g. `Tag("Material")`, only the matching arm
    /// needs to be lowered, eliminating dead arms and dramatically reducing IR size
    /// for multi-file programs like todo_mvc_physical (90 Theme calls × 4 themes × 11 categories).
    constant_cells: HashMap<CellId, IrValue>,

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
            pending_list_constructor: None,
            current_var_name: None,
            current_passed: None,
            passed_resolve_depth: 0,
            inline_depth: 0,
            force_object_store: false,
            constant_cells: HashMap::new(),
            pending_per_item_removes: HashMap::new(),
        }
    }

    /// Register external functions from parsed module files.
    fn register_external_functions(&mut self, ext_fns: &[ExternalFunction]) {
        for (qualified_name, params, body, module_name) in ext_fns {
            let func_id = FuncId(u32::try_from(self.functions.len()).unwrap());
            self.name_to_func
                .insert(qualified_name.clone(), func_id);
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
            for node in &self.nodes {
                match node {
                    IrNode::Derived {
                        cell: c,
                        expr: IrExpr::CellRead(src),
                    } if *c == current => {
                        found_source = Some(*src);
                        break;
                    }
                    IrNode::PipeThrough { cell: c, source } if *c == current => {
                        found_source = Some(*source);
                        break;
                    }
                    IrNode::ListRetain { cell: c, source, .. } if *c == current => {
                        found_source = Some(*source);
                        break;
                    }
                    IrNode::ListRemove { cell: c, source, .. } if *c == current => {
                        found_source = Some(*source);
                        break;
                    }
                    IrNode::ListAppend { cell: c, source, .. } if *c == current => {
                        found_source = Some(*source);
                        break;
                    }
                    IrNode::ListClear { cell: c, source, .. } if *c == current => {
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
        if let Some(c) = self.list_item_constructor.get(&source).cloned() {
            self.list_item_constructor.insert(target, c);
        }
        // Also propagate pending per-item removes along the list chain.
        if let Some(removes) = self.pending_per_item_removes.get(&source).cloned() {
            self.pending_per_item_removes
                .entry(target)
                .or_default()
                .extend(removes);
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
            None => {
                return false;
            }
        };
        let func_def = match self.func_defs.get(&constructor_name).cloned() {
            Some(def) => def,
            None => {
                return false;
            }
        };

        // Save bindings for constructor params.
        let mut saved_bindings: Vec<(String, Option<CellId>)> = Vec::new();
        for param in &func_def.params {
            saved_bindings.push((param.clone(), self.name_to_cell.get(param).copied()));
        }

        // Bind first param (title) to item_cell.
        if let Some(first) = func_def.params.first() {
            self.name_to_cell.insert(first.clone(), item_cell);
        }

        // Bind remaining params to defaults (False).
        for param in func_def.params.iter().skip(1) {
            let default_cell = self.alloc_cell(param, span);
            self.nodes.push(IrNode::Derived {
                cell: default_cell,
                expr: IrExpr::Constant(IrValue::Bool(false)),
            });
            self.name_to_cell.insert(param.clone(), default_cell);
        }

        // Lower constructor body — creates namespace cells in template range.
        // Force objects to use lower_object_store so field cells are always created.
        let saved_force = self.force_object_store;
        self.force_object_store = true;
        let result = self.lower_expr(&func_def.body.node, func_def.body.span);
        self.force_object_store = saved_force;

        // If result is a namespace cell, bind item_name to it and propagate field cells.
        if let IrExpr::CellRead(result_cell) = &result {
            // Bind item name to the constructor's result cell so template
            // can resolve field accesses (e.g., todo.editing) through it.
            self.name_to_cell
                .insert(item_name.to_string(), *result_cell);

            // Also propagate cell_field_cells to item_cell for bridge use.
            if let Some(fields) = self.cell_field_cells.get(result_cell).cloned() {
                self.cell_field_cells.insert(item_cell, fields);
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
        Ok(IrProgram {
            cells: self.cells,
            events: self.events,
            nodes: self.nodes,
            document: self.document,
            functions: self.functions,
            tag_table: self.tag_table,
            cell_field_cells: self.cell_field_cells,
        })
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn lower(
    ast: &[Spanned<Expression>],
    external_functions: Option<&[ExternalFunction]>,
) -> Result<IrProgram, Vec<CompileError>> {
    let mut ctx = Lowerer::new();

    // Register external functions from other module files
    if let Some(ext_fns) = external_functions {
        ctx.register_external_functions(ext_fns);
    }

    // --- Pass 1: Register top-level names ---
    // Pre-allocate CellIds for all top-level variables so forward references work.
    let mut top_level_vars: Vec<(&Variable, Span)> = Vec::new();

    // Also collect bare top-level FUNCTION definitions.
    let mut top_level_functions: Vec<(&Spanned<Expression>, Span)> = Vec::new();

    for item in ast {
        match &item.node {
            Expression::Variable(var) => {
                let name = var.name.as_str();
                // Check for function definitions inside variable assignment
                if let Expression::Function {
                    name: fn_name,
                    parameters,
                    body,
                } = &var.value.node
                {
                    let func_id = FuncId(u32::try_from(ctx.functions.len()).unwrap());
                    let fn_name_str = fn_name.as_str().to_string();
                    ctx.name_to_func.insert(fn_name_str.clone(), func_id);
                    // Store AST for inlining at call sites.
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
            // Bare top-level FUNCTION definition (not assigned to a variable).
            Expression::Function {
                name: fn_name,
                parameters,
                body,
            } => {
                let func_id = FuncId(u32::try_from(ctx.functions.len()).unwrap());
                let fn_name_str = fn_name.as_str().to_string();
                ctx.name_to_func.insert(fn_name_str.clone(), func_id);
                // Store AST for inlining at call sites.
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
                top_level_functions.push((item, item.span));
            }
            _ => {
                ctx.error(
                    item.span,
                    "Top-level expression must be a variable or function definition",
                );
            }
        }
    }

    // --- Pass 1.5: Pre-scan element definitions for LINK events ---
    for (var, span) in &top_level_vars {
        let var_name = var.name.as_str().to_string();
        let cell = ctx.name_to_cell[var.name.as_str()];
        pre_scan_links_in_expr(&var.value.node, &var_name, cell, *span, &mut ctx);
    }

    // --- Pass 2: Lower each variable ---
    for (var, span) in &top_level_vars {
        let name = var.name.as_str();
        let cell = ctx.name_to_cell[name];

        // Check for function definitions — lower the body into IrFunction.
        if let Expression::Function {
            name: fn_name,
            parameters,
            body,
        } = &var.value.node
        {
            // Functions are inlined at call sites — don't eagerly lower bodies
            // since PASS/PASSED context is only available at call time.
            ctx.nodes.push(IrNode::Derived {
                cell,
                expr: IrExpr::Constant(IrValue::Void),
            });
            continue;
        }

        ctx.lower_variable(cell, &var.value, *span);
    }

    // Bare top-level functions are inlined at call sites.
    // Don't eagerly lower their bodies here — PASS/PASSED context
    // is only available at the call site during inlining.

    ctx.finish()
}

// ---------------------------------------------------------------------------
// Expression lowering
// ---------------------------------------------------------------------------

impl Lowerer {
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
                    let expr = self.lower_expr(&value.node, value.span);
                    self.nodes.push(IrNode::Derived { cell, expr });
                }
            }

            // --- Object-as-store pattern ---
            Expression::Object(obj) => {
                self.lower_object_store(cell, obj, var_span);
            }

            // --- Simple expression → Derived ---
            _ => {
                let expr = self.lower_expr(&value.node, value.span);
                // Consume pending_list_constructor for top-level LIST variables.
                if matches!(&expr, IrExpr::ListConstruct(_)) {
                    if let Some(constructor_name) = self.pending_list_constructor.take() {
                        self.list_item_constructor.insert(cell, constructor_name);
                    }
                }
                // Propagate cell_field_cells so object fields remain accessible.
                if let IrExpr::CellRead(src) = &expr {
                    if let Some(fields) = self.cell_field_cells.get(src).cloned() {
                        self.cell_field_cells.insert(cell, fields);
                    }
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
                    .and_then(|v| {
                        let expr = self.lower_expr(&v.node, v.span);
                        match expr {
                            IrExpr::CellRead(c) => Some(c),
                            _ => None,
                        }
                    });
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
                    if let Some(d) = self.find_field_in_arg_object(arguments, "style", "direction") {
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
                let label = self.find_arg_expr(arguments, "text", span);
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
                let cx = self.find_arg_expr_or(
                    arguments,
                    "cx",
                    IrExpr::Constant(IrValue::Number(0.0)),
                );
                let cy = self.find_arg_expr_or(
                    arguments,
                    "cy",
                    IrExpr::Constant(IrValue::Number(0.0)),
                );
                let r = self.find_arg_expr_or(
                    arguments,
                    "r",
                    IrExpr::Constant(IrValue::Number(20.0)),
                );
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
                            if let Some(sub_fields) =
                                self.cell_field_cells.get(field_cell).cloned()
                            {
                                self.cell_field_cells.insert(alias_cell, sub_fields);
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
            // Short name will be registered after value lowering.
            self.name_to_cell.insert(dotted_name, field_cell);

            field_cells.push((field_name, field_cell));
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
            let (field_name, field_cell) = &field_cells[field_idx];
            field_idx += 1;

            if matches!(v.node.value.node, Expression::Link) {
                // LINK placeholder — pre-allocate events and data cells.
                self.pre_allocate_link_events(*field_cell, v.span);
                self.nodes.push(IrNode::Derived {
                    cell: *field_cell,
                    expr: IrExpr::Constant(IrValue::Void),
                });
            } else {
                self.lower_variable(*field_cell, &v.node.value, v.span);
            }
            // Register the short field name AFTER lowering so this field's own
            // value doesn't self-reference, but subsequent fields CAN reference it.
            self.name_to_cell.insert(field_name.clone(), *field_cell);
        }

        // Register field cells: merge spread fields (first) and explicit fields (override).
        let mut field_map: HashMap<String, CellId> =
            spread_field_cells.iter().cloned().collect();
        for (name, cell) in &field_cells {
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
        self.element_events.insert(cell_name.clone(), events);

        // Pre-allocate data cells for event payloads.
        // key_down.key → CellId (stores the key tag, e.g., Enter, Escape)
        let key_cell_name = format!("{}.event.key_down.key", cell_name);
        let key_cell = self.alloc_cell(&key_cell_name, span);
        self.name_to_cell.insert(key_cell_name, key_cell);
        self.nodes.push(IrNode::Derived {
            cell: key_cell,
            expr: IrExpr::Constant(IrValue::Void),
        });
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

        // change.text → CellId (stores the changed text)
        let change_text_name = format!("{}.event.change.text", cell_name);
        let change_text_cell = self.alloc_cell(&change_text_name, span);
        self.name_to_cell.insert(change_text_name, change_text_cell);
        self.nodes.push(IrNode::Derived {
            cell: change_text_cell,
            expr: IrExpr::Constant(IrValue::Void),
        });
        // Add change_text_cell as payload for the change event.
        if let Some(&event_id) = self
            .element_events
            .get(&cell_name)
            .and_then(|evts| evts.get("change"))
        {
            self.cell_events.insert(change_text_cell, event_id);
            self.events[event_id.0 as usize]
                .payload_cells
                .push(change_text_cell);
        }

        // .text → CellId (current text value of the input)
        let text_name = format!("{}.text", cell_name);
        let text_cell = self.alloc_cell(&text_name, span);
        self.name_to_cell.insert(text_name, text_cell);
        self.nodes.push(IrNode::Derived {
            cell: text_cell,
            expr: IrExpr::Constant(IrValue::Void),
        });
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
                                    self.cell_events.insert(text_cell, event_id);
                                    self.events[event_id.0 as usize]
                                        .payload_cells
                                        .push(text_cell);
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

        SavedElementBindings {
            bindings: saved_bindings,
            events: saved_events,
            hovered_cell,
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

        // Special case: LinkSetter needs to set current_var_name BEFORE
        // lowering `from`, so the element picks up the LINK target's events.
        if let Expression::LinkSetter { alias } = &to.node {
            let target_name = self.resolve_link_target_name(&alias.node, alias.span);
            if let Some(ref name) = target_name {
                let saved_var_name = self.current_var_name.take();
                self.current_var_name = Some(name.clone());
                let source_cell = self.lower_expr_to_cell(from, "pipe_from");
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
        let effective = if path_strs.len() == 3
            && path_strs[0] == "Scene"
            && path_strs[1] == "Element"
        {
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
                let root_cell = self.lower_expr_to_cell(source, "doc_root");
                // Also check for a `root:` argument.
                let root = self.find_arg_cell(arguments, "root", source, call_span);
                self.nodes.push(IrNode::Document { root });
                self.document = Some(root);
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
                        // Lower the template expression.
                        let template_expr = self.lower_expr(&val.node, val.span);
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
                        return;
                    }
                }
                self.nodes.push(IrNode::PipeThrough {
                    cell: target,
                    source: source_cell,
                });
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
                self.nodes.push(IrNode::PipeThrough {
                    cell: target,
                    source: source_cell,
                });
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

            // --- `source |> Text/starts_with(prefix: ...)` ---
            ["Text", "starts_with"] => {
                let source_cell = self.lower_expr_to_cell(source, "starts_with_source");
                let prefix_cell = if let Some(prefix_arg) = arguments.iter().find(|a| a.node.name.as_str() == "prefix") {
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
                let when_arg = arguments.iter()
                    .find(|a| a.node.name.as_str() == "when");
                if let Some(when_arg) = when_arg {
                    if let Some(ref val) = when_arg.node.value {
                        // Try event resolution from expression FIRST (handles
                        // element.event.click paths), then fall back to cell-based.
                        let trigger = self.resolve_event_from_expr(&val.node)
                            .unwrap_or_else(|| {
                                let when_cell = self.lower_expr_to_cell(val, "toggle_when");
                                self.resolve_event_from_cell(when_cell)
                                    .unwrap_or_else(|| {
                                        self.alloc_event("toggle_trigger", EventSource::Synthetic, call_span)
                                    })
                            });
                        self.nodes.push(IrNode::Hold {
                            cell: target,
                            init: IrExpr::CellRead(source_cell),
                            trigger_bodies: vec![(trigger, IrExpr::Not(Box::new(IrExpr::CellRead(target))))],
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
                    // Set module context for intra-module resolution during body inlining
                    self.current_module = self.function_modules.get(&fn_name).cloned();
                    let source_cell = self.lower_expr_to_cell(source, "pipe_fn_arg");
                    let result = self.inline_function_call_with_pipe(
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
                            let expr = self.lower_pipe_to_expr(from, to);
                            arms.push(LatestArm {
                                trigger: None,
                                body: expr,
                            });
                        }
                    }
                }
                _ => {
                    // Static value arm (e.g., the initial `0` in counter).
                    let expr = self.lower_expr(&input.node, input.span);
                    arms.push(LatestArm {
                        trigger: None,
                        body: expr,
                    });
                }
            }
        }
        self.nodes.push(IrNode::Latest { target, arms });
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
                let trigger = self
                    .resolve_event_from_expr(body)
                    .unwrap_or_else(|| {
                        self.alloc_event("hold_trigger", EventSource::Synthetic, hold_span)
                    });
                let expr = self.lower_expr(body, body_span);
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
        self.cell_events.get(&cell).copied()
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

                while let Some(part) = parts_iter.next() {
                    let field = part.as_str();
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
                            // Resolve alias to get cell name, then append remaining parts.
                            if let Alias::WithoutPassed {
                                parts: alias_parts, ..
                            } = inner_alias
                            {
                                if let Some(&cell) = self.name_to_cell.get(alias_parts[0].as_str())
                                {
                                    let mut name = self.cells[cell.0 as usize].name.clone();
                                    name.push('.');
                                    name.push_str(field);
                                    for remaining in parts_iter {
                                        name.push('.');
                                        name.push_str(remaining.as_str());
                                    }
                                    return Some(name);
                                }
                            }
                            return None;
                        }
                        _ => return None,
                    }
                }

                // If we consumed all parts and landed on a Link placeholder, find its cell name.
                if matches!(current_expr, Expression::Link) {
                    // We've navigated to a LINK node. We need to find which cell this is.
                    // The cell was created during object flattening. Try to find it
                    // by checking the lowered alias.
                    None
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

    /// Resolve a multi-part alias (e.g., "item.item_elements.toggle_button")
    /// through the cell hierarchy to find the actual cell name for element_events lookup.
    fn resolve_link_alias_through_cells(
        &self,
        parts: &[crate::parser::StrSlice],
    ) -> Option<String> {
        // Start with the first part and find its cell.
        let first = parts[0].as_str();
        let mut current_cell = self.name_to_cell.get(first).copied()?;

        // Follow remaining parts through cell_field_cells.
        for part in &parts[1..] {
            let field = part.as_str();
            // Check cell_field_cells for a direct field lookup.
            if let Some(fields) = self.cell_field_cells.get(&current_cell) {
                if let Some(&field_cell) = fields.get(field) {
                    current_cell = field_cell;
                    continue;
                }
            }
            // Also try global name + field in name_to_cell.
            let cell_name = &self.cells[current_cell.0 as usize].name;
            let global_path = format!("{}.{}", cell_name, field);
            if let Some(&cell) = self.name_to_cell.get(&global_path) {
                current_cell = cell;
            } else {
                return None;
            }
        }

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
                for v in variables {
                    let name = v.node.name.as_str();
                    let cell = self.alloc_cell(name, v.span);
                    self.name_to_cell.insert(name.to_string(), cell);
                    let expr = self.lower_expr(&v.node.value.node, v.node.value.span);
                    // Propagate cell_field_cells so object-store fields remain
                    // accessible through this BLOCK variable.
                    if let IrExpr::CellRead(src) = &expr {
                        if let Some(fields) = self.cell_field_cells.get(src).cloned() {
                            self.cell_field_cells.insert(cell, fields);
                        }
                    }
                    self.nodes.push(IrNode::Derived { cell, expr });
                }
                self.lower_expr(&output.node, output.span)
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
                IrExpr::FieldAccess {
                    object: Box::new(inner),
                    field: field.as_str().to_string(),
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
        let effective = if path_strs.len() == 3
            && path_strs[0] == "Scene"
            && path_strs[1] == "Element"
        {
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
                        self.nodes.push(IrNode::Document { root: root_cell });
                        self.document = Some(root_cell);
                        return IrExpr::CellRead(root_cell);
                    }
                }
                self.error(span, "Document/new requires a 'root' argument");
                IrExpr::Constant(IrValue::Void)
            }

            // --- Scene/new(root: ..., lights: ..., geometry: ...) ---
            // Extract the root element and treat it as the document root.
            // Lights and geometry are ignored (no physical CSS in WASM engine yet).
            ["Scene", "new"] => {
                if let Some(root_arg) = arguments.iter().find(|a| a.node.name.as_str() == "root") {
                    if let Some(ref val) = root_arg.node.value {
                        let root_cell = self.lower_expr_to_cell(val, "scene_root");
                        self.nodes.push(IrNode::Document { root: root_cell });
                        self.document = Some(root_cell);
                        return IrExpr::CellRead(root_cell);
                    }
                }
                self.error(span, "Scene/new requires a 'root' argument");
                IrExpr::Constant(IrValue::Void)
            }

            // --- Lights/basic(), Light/directional(), Light/ambient() ---
            // Stubs — values unused without physical CSS.
            ["Lights", "basic"] | ["Lights", "directional"] | ["Lights", "ambient"]
            | ["Light", "directional"] | ["Light", "ambient"] => {
                IrExpr::Constant(IrValue::Void)
            }

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
                let text_expr = arguments
                    .iter()
                    .find(|a| a.node.name.as_str() == "text")
                    .and_then(|a| a.node.value.as_ref())
                    .map(|v| self.lower_expr(&v.node, v.span));

                let cell = self.alloc_cell("text_input", span);
                let links =
                    self.extract_links_for_element(&self.cells[cell.0 as usize].name.clone());
                let hovered_cell = saved_elem.hovered_cell;

                let focus = self.has_element_bool_field(arguments, "focus")
                    || self.has_top_level_bool_arg(arguments, "focus");
                let text_cell = text_expr.and_then(|expr| match expr {
                    IrExpr::CellRead(c) => Some(c),
                    _ => None,
                });
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
                let cx = self.find_arg_expr_or(
                    arguments,
                    "cx",
                    IrExpr::Constant(IrValue::Number(0.0)),
                );
                let cy = self.find_arg_expr_or(
                    arguments,
                    "cy",
                    IrExpr::Constant(IrValue::Number(0.0)),
                );
                let r = self.find_arg_expr_or(
                    arguments,
                    "r",
                    IrExpr::Constant(IrValue::Number(20.0)),
                );
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
                    if let Some(d) = self.find_field_in_arg_object(arguments, "style", "direction") {
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
                    self.current_module = self.function_modules.get(&fn_name).cloned();
                    return self.inline_function_call(&func_def, arguments, span);
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
                // Track constant values for compile-time WHEN folding.
                if let IrExpr::Constant(ref v) = ir_expr {
                    self.constant_cells.insert(cell, v.clone());
                }
                // TaggedObject: tag is constant; create sub-cells for field
                // destructuring in WHEN patterns.
                if let IrExpr::TaggedObject { ref tag, ref fields } = ir_expr {
                    self.constant_cells
                        .insert(cell, IrValue::Tag(tag.clone()));
                    let mut field_map = HashMap::new();
                    for (field_name, field_expr) in fields {
                        let field_cell = match field_expr {
                            IrExpr::CellRead(c) => *c,
                            _ => {
                                let fc = self.alloc_cell(field_name, expr.span);
                                self.nodes.push(IrNode::Derived {
                                    cell: fc,
                                    expr: field_expr.clone(),
                                });
                                fc
                            }
                        };
                        field_map.insert(field_name.clone(), field_cell);
                    }
                    self.cell_field_cells.insert(cell, field_map);
                }
                self.nodes.push(IrNode::Derived {
                    cell,
                    expr: ir_expr,
                });
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

                let first = parts[0].as_str();


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
                                        if let Some(&_event_id) = events.get(event_name) {
                                            if rest.len() == 2 {
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
                                if let Some(&_event_id) = events.get(event_name) {
                                    if parts.len() == 3 {
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
                        let mut expr = IrExpr::CellRead(cell);
                        for part in &parts[1..] {
                            expr = IrExpr::FieldAccess {
                                object: Box::new(expr),
                                field: part.as_str().to_string(),
                            };
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
        func_def: &FuncDef,
        arguments: &[Spanned<Argument>],
        span: Span,
    ) -> IrExpr {
        self.inline_depth += 1;
        if self.inline_depth > 64 {
            self.inline_depth -= 1;
            self.error(span, "Function inlining exceeded maximum depth (possible recursion)");
            return IrExpr::Constant(IrValue::Void);
        }

        // Save entire name_to_cell state so BLOCK variables created during body
        // lowering don't leak into the caller's scope. Without this, a function
        // like `text()` that inlines `font()` (which also has a `small_base` BLOCK
        // variable) would see font's `small_base` overwrite text's `small_base`.
        let saved_names = self.name_to_cell.clone();

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
                let expr = self.lower_expr(&val.node, val.span);

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
                let tagged_field_cells =
                    if let IrExpr::TaggedObject { ref fields, .. } = expr {
                        let mut field_map = HashMap::new();
                        for (field_name, field_expr) in fields {
                            let field_cell = match field_expr {
                                IrExpr::CellRead(c) => *c,
                                _ => {
                                    let fc =
                                        self.alloc_cell(field_name, span);
                                    self.nodes.push(IrNode::Derived {
                                        cell: fc,
                                        expr: field_expr.clone(),
                                    });
                                    fc
                                }
                            };
                            field_map.insert(field_name.clone(), field_cell);
                        }
                        Some(field_map)
                    } else {
                        None
                    };

                // If the argument is a namespace cell (object with field cells),
                // propagate field cell names under the parameter name prefix so
                // `param.field` paths resolve correctly (e.g., `todo.title`).
                // Must extract source_cell BEFORE moving expr into Derived node.
                let namespace_source = if let IrExpr::CellRead(source_cell) = &expr {
                    Some(*source_cell)
                } else {
                    None
                };
                self.nodes.push(IrNode::Derived { cell, expr });
                self.name_to_cell.insert(param_name.clone(), cell);

                if let Some(cv) = constant_value {
                    self.constant_cells.insert(cell, cv);
                }

                if let Some(field_map) = tagged_field_cells {
                    for (field_name, field_cell) in &field_map {
                        let dotted = format!("{}.{}", param_name, field_name);
                        self.name_to_cell.insert(dotted, *field_cell);
                    }
                    self.cell_field_cells.insert(cell, field_map);
                } else if let Some(source_cell) = namespace_source {
                    if let Some(fields) = self.cell_field_cells.get(&source_cell).cloned() {
                        for (field_name, field_cell) in &fields {
                            let dotted = format!("{}.{}", param_name, field_name);
                            self.name_to_cell.insert(dotted, *field_cell);
                        }
                        self.cell_field_cells.insert(cell, fields);
                    }
                }
            }
        }

        // Lower the function body.
        let result = self.lower_expr(&func_def.body.node, func_def.body.span);

        // Restore entire name_to_cell state (undoes both parameter bindings
        // and any BLOCK variables created during body lowering).
        self.name_to_cell = saved_names;

        // Restore previous PASSED context.
        self.current_passed = saved_passed;
        // Restore previous module context.
        self.current_module = saved_module;

        self.inline_depth -= 1;
        result
    }

    /// Inline a user-defined function call with a piped argument.
    /// The piped value becomes the first parameter.
    /// `outer_var_name` is used to propagate LINK events from the outer variable
    /// to elements created inside the function body.
    fn inline_function_call_with_pipe(
        &mut self,
        func_def: &FuncDef,
        pipe_source: CellId,
        arguments: &[Spanned<Argument>],
        span: Span,
    ) -> IrExpr {
        self.inline_depth += 1;
        if self.inline_depth > 64 {
            self.inline_depth -= 1;
            self.error(span, "Function inlining exceeded maximum depth (possible recursion)");
            return IrExpr::Constant(IrValue::Void);
        }

        // Save entire name_to_cell state so BLOCK variables created during body
        // lowering don't leak into the caller's scope (same rationale as
        // inline_function_call).
        let saved_names = self.name_to_cell.clone();

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
            self.name_to_cell.insert(first_param.clone(), pipe_source);

            // If the piped source is a namespace cell, propagate field cell
            // names so `param.field` paths resolve (e.g., `todo.title`).
            if let Some(fields) = self.cell_field_cells.get(&pipe_source).cloned() {
                for (field_name, field_cell) in &fields {
                    let dotted = format!("{}.{}", first_param, field_name);
                    self.name_to_cell.insert(dotted, *field_cell);
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
                let expr = self.lower_expr(&val.node, val.span);

                // Track constant values for compile-time WHEN/WHILE folding.
                // TaggedObject: tag is constant even when fields are dynamic.
                let constant_value = match &expr {
                    IrExpr::Constant(v) => Some(v.clone()),
                    IrExpr::CellRead(src) => self.constant_cells.get(src).cloned(),
                    IrExpr::TaggedObject { tag, .. } => Some(IrValue::Tag(tag.clone())),
                    _ => None,
                };

                // For TaggedObject, create sub-cells for field destructuring.
                let tagged_field_cells =
                    if let IrExpr::TaggedObject { ref fields, .. } = expr {
                        let mut field_map = HashMap::new();
                        for (field_name, field_expr) in fields {
                            let field_cell = match field_expr {
                                IrExpr::CellRead(c) => *c,
                                _ => {
                                    let fc =
                                        self.alloc_cell(field_name, span);
                                    self.nodes.push(IrNode::Derived {
                                        cell: fc,
                                        expr: field_expr.clone(),
                                    });
                                    fc
                                }
                            };
                            field_map.insert(field_name.clone(), field_cell);
                        }
                        Some(field_map)
                    } else {
                        None
                    };

                // Extract namespace source BEFORE moving expr into Derived node.
                let namespace_source = if let IrExpr::CellRead(source_cell) = &expr {
                    Some(*source_cell)
                } else {
                    None
                };
                self.nodes.push(IrNode::Derived { cell, expr });
                self.name_to_cell.insert(param_name.clone(), cell);

                if let Some(cv) = constant_value {
                    self.constant_cells.insert(cell, cv);
                }

                // Propagate namespace field cell names (same as inline_function_call).
                if let Some(field_map) = tagged_field_cells {
                    for (field_name, field_cell) in &field_map {
                        let dotted = format!("{}.{}", param_name, field_name);
                        self.name_to_cell.insert(dotted, *field_cell);
                    }
                    self.cell_field_cells.insert(cell, field_map);
                } else if let Some(source_cell) = namespace_source {
                    if let Some(fields) = self.cell_field_cells.get(&source_cell).cloned() {
                        for (field_name, field_cell) in &fields {
                            let dotted = format!("{}.{}", param_name, field_name);
                            self.name_to_cell.insert(dotted, *field_cell);
                        }
                        self.cell_field_cells.insert(cell, fields);
                    }
                }
            }
        }

        // Lower the function body.
        let result = self.lower_expr(&func_def.body.node, func_def.body.span);

        // Restore entire name_to_cell state (undoes both parameter bindings
        // and any BLOCK variables created during body lowering).
        self.name_to_cell = saved_names;

        // Restore previous PASSED context.
        self.current_passed = saved_passed;
        // Restore previous module context.
        self.current_module = saved_module;

        self.inline_depth -= 1;
        result
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
        if let Some(fields) = self.cell_field_cells.get(&cell) {
            return Some(fields.clone());
        }
        // Follow the chain through Derived(CellRead), Derived(FieldAccess), and PipeThrough nodes.
        for node in &self.nodes {
            match node {
                IrNode::Derived {
                    cell: c,
                    expr: IrExpr::CellRead(src),
                } if *c == cell => {
                    return self.resolve_cell_field_cells(*src);
                }
                IrNode::Derived {
                    cell: c,
                    expr: IrExpr::FieldAccess { object, field },
                } if *c == cell => {
                    // Follow FieldAccess: resolve the object's field cells, find the field,
                    // and recursively resolve the target field cell's sub-fields.
                    if let IrExpr::CellRead(obj_cell) = object.as_ref() {
                        if let Some(obj_fields) = self.resolve_cell_field_cells(*obj_cell) {
                            if let Some(&field_cell) = obj_fields.get(field.as_str()) {
                                return self.resolve_cell_field_cells(field_cell);
                            }
                        }
                    }
                    return None;
                }
                IrNode::PipeThrough { cell: c, source } if *c == cell => {
                    return self.resolve_cell_field_cells(*source);
                }
                _ => {}
            }
        }
        None
    }

    /// Follow CellRead/Derived/PipeThrough chains to find an inline object or
    /// TaggedObject expression. Returns the fields if found.
    fn resolve_cell_to_inline_object(&self, cell: CellId) -> Option<Vec<(String, IrExpr)>> {
        for node in &self.nodes {
            match node {
                IrNode::Derived {
                    cell: c,
                    expr: IrExpr::ObjectConstruct(fields),
                } if *c == cell => return Some(fields.clone()),
                IrNode::Derived {
                    cell: c,
                    expr: IrExpr::TaggedObject { fields, .. },
                } if *c == cell => return Some(fields.clone()),
                IrNode::Derived {
                    cell: c,
                    expr: IrExpr::CellRead(src),
                } if *c == cell => return self.resolve_cell_to_inline_object(*src),
                IrNode::Derived {
                    cell: c,
                    expr: IrExpr::FieldAccess { object, field },
                } if *c == cell => {
                    // Follow FieldAccess through object's cell_field_cells.
                    if let IrExpr::CellRead(obj_cell) = object.as_ref() {
                        if let Some(obj_fields) = self.resolve_cell_field_cells(*obj_cell) {
                            if let Some(&field_cell) = obj_fields.get(field.as_str()) {
                                return self.resolve_cell_to_inline_object(field_cell);
                            }
                        }
                    }
                    return None;
                }
                IrNode::PipeThrough { cell: c, source } if *c == cell => {
                    return self.resolve_cell_to_inline_object(*source)
                }
                _ => {}
            }
        }
        None
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
            IrNode::When { cell: c, source, arms } if *c == cell => {
                Some((*source, arms.clone()))
            }
            IrNode::While { cell: c, source, arms, .. } if *c == cell => {
                Some((*source, arms.clone()))
            }
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
            if matches!(body, IrExpr::Constant(IrValue::Skip) | IrExpr::Constant(IrValue::Void)) {
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
                                if let Some(inner_fields) = self.resolve_cell_field_cells(field_cell) {
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
                                } else if let Some(inline) = self.resolve_cell_to_inline_object(field_cell) {
                                    inline
                                } else if self.ensure_cell_distributed(field_cell) {
                                    if let Some(sub_fields) = self.resolve_cell_field_cells(field_cell) {
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
                        self.name_to_cell
                            .insert(field_name.to_string(), field_cell);
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
            match path_strs.as_slice() {
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

    /// Extract a field from within a named argument's object value.
    /// Used when fields like `direction` or `gap` are nested inside `style: [direction: Right, ...]`.
    fn find_field_in_arg_object(
        &mut self,
        arguments: &[Spanned<Argument>],
        arg_name: &str,
        field_name: &str,
    ) -> Option<IrExpr> {
        let arg = arguments.iter().find(|a| a.node.name.as_str() == arg_name)?;
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
                let trigger = self
                    .resolve_event_from_cell(source_cell)
                    .unwrap_or_else(|| {
                        self.alloc_event("then_trigger", EventSource::Synthetic, to.span)
                    });
                let body_expr = self.lower_expr(&body.node, body.span);
                self.nodes.push(IrNode::Then {
                    cell: target,
                    trigger,
                    body: body_expr,
                });
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
                self.nodes.push(IrNode::Hold {
                    cell: target,
                    init: init_expr,
                    trigger_bodies,
                });
            }

            Expression::When { arms } => {
                // Constant folding: if source is a known constant tag/value,
                // only lower the matching arm (skip all others).
                if let Some(const_val) = self.constant_cells.get(&source_cell).cloned() {
                    let mut folded = false;
                    for arm in arms {
                        if self.pattern_matches_constant(&arm.pattern, &const_val) {
                            // Bind destructured fields from TaggedObject patterns.
                            // E.g., for `ButtonIcon[checked] => body`, bind `checked`
                            // to the source cell's "checked" field cell.
                            let saved_bindings = self
                                .bind_tagged_pattern_fields(&arm.pattern, source_cell);

                            let body = self.lower_expr(&arm.body.node, arm.body.span);

                            // Restore bindings.
                            self.unbind_pattern_fields(saved_bindings);

                            // Propagate cell_field_cells through the Derived node
                            // so the bridge can resolve object fields downstream.
                            if let IrExpr::CellRead(src) = &body {
                                if let Some(fields) =
                                    self.resolve_cell_field_cells(*src)
                                {
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
                        self.nodes.push(IrNode::Document { root });
                        self.document = Some(root);
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
                    ["Stream", "pulses"] | ["Stream", "skip"] => {
                        self.nodes.push(IrNode::PipeThrough {
                            cell: target,
                            source: source_cell,
                        });
                        // Propagate field cell info through pass-through.
                        if let Some(fields) = self.cell_field_cells.get(&source_cell).cloned() {
                            self.cell_field_cells.insert(target, fields);
                        }
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
                                let template_expr = self.lower_expr(&val.node, val.span);
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
                                return;
                            }
                        }
                        self.nodes.push(IrNode::PipeThrough {
                            cell: target,
                            source: source_cell,
                        });
                    }
                    ["List", "is_empty"] => {
                        self.nodes.push(IrNode::ListIsEmpty {
                            cell: target,
                            source: source_cell,
                        });
                    }
                    ["List", "is_not_empty"] => {
                        let is_empty_cell =
                            self.alloc_cell("list_is_empty_intermediate", var_span);
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
                    ["Text", "is_not_empty"] => {
                        self.nodes.push(IrNode::TextIsNotEmpty {
                            cell: target,
                            source: source_cell,
                        });
                    }
                    ["Text", "starts_with"] => {
                        let prefix_cell = if let Some(prefix_arg) = arguments.iter().find(|a| a.node.name.as_str() == "prefix") {
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
                    ["Bool", "not"] => {
                        self.nodes.push(IrNode::Derived {
                            cell: target,
                            expr: IrExpr::Not(Box::new(IrExpr::CellRead(source_cell))),
                        });
                    }
                    // --- `source |> Bool/toggle(when: event)` ---
                    ["Bool", "toggle"] => {
                        let when_arg = arguments.iter()
                            .find(|a| a.node.name.as_str() == "when");
                        if let Some(when_arg) = when_arg {
                            if let Some(ref val) = when_arg.node.value {
                                // Try event resolution from expression FIRST (handles
                                // element.event.click paths), then fall back to cell-based.
                                let trigger = self.resolve_event_from_expr(&val.node)
                                    .unwrap_or_else(|| {
                                        let when_cell = self.lower_expr_to_cell(val, "toggle_when");
                                        self.resolve_event_from_cell(when_cell)
                                            .unwrap_or_else(|| {
                                                self.alloc_event("toggle_trigger", EventSource::Synthetic, to.span)
                                            })
                                    });
                                self.nodes.push(IrNode::Hold {
                                    cell: target,
                                    init: IrExpr::CellRead(source_cell),
                                    trigger_bodies: vec![(trigger, IrExpr::Not(Box::new(IrExpr::CellRead(target))))],
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
                            self.current_module = self.function_modules.get(&fn_name).cloned();
                            let result = self.inline_function_call_with_pipe(
                                &func_def,
                                source_cell,
                                arguments,
                                to.span,
                            );
                            match result {
                                IrExpr::CellRead(result_cell) => {
                                    self.nodes.push(IrNode::PipeThrough {
                                        cell: target,
                                        source: result_cell,
                                    });
                                    // Propagate events from result cell.
                                    if let Some(event) =
                                        self.cell_events.get(&result_cell).copied()
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
        if let Some(events) = self.element_events.get(element_var_name) {
            events
                .iter()
                .map(|(name, &eid)| (name.clone(), eid))
                .collect()
        } else if let Some(ref outer_name) = self.current_var_name {
            // Fallback: try the outer variable name (for elements inside function calls).
            if let Some(events) = self.element_events.get(outer_name) {
                events
                    .iter()
                    .map(|(name, &eid)| (name.clone(), eid))
                    .collect()
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
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
                || (path_strs.len() == 3
                    && path_strs[0] == "Scene"
                    && path_strs[1] == "Element");
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
