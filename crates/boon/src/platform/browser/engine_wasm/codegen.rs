//! IR → WASM binary code generation using wasm-encoder.
//!
//! Generates a WASM module with:
//! - One mutable f64 global per cell (cell values)
//! - Host imports for DOM updates: host_set_cell_f64, host_set_cell_text, host_log
//! - `init()` — sets initial cell values, calls host emit for initial render
//! - `on_event(event_id: i32)` — dispatches events, updates cells, re-emits

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use wasm_encoder::{
    CodeSection, ConstExpr, ExportKind, ExportSection, Function, FunctionSection, GlobalSection,
    GlobalType, ImportSection, Instruction, MemorySection, MemoryType, Module, TypeSection,
    ValType,
};

use super::ir::*;

// ---------------------------------------------------------------------------
// Host import indices (must match order of imports)
// ---------------------------------------------------------------------------

const IMPORT_HOST_SET_CELL_F64: u32 = 0;
const IMPORT_HOST_NOTIFY_INIT_DONE: u32 = 1;
const IMPORT_HOST_LIST_CREATE: u32 = 2;
const IMPORT_HOST_LIST_APPEND: u32 = 3;
const IMPORT_HOST_LIST_CLEAR: u32 = 4;
const IMPORT_HOST_LIST_COUNT: u32 = 5;
const IMPORT_HOST_TEXT_TRIM: u32 = 6;
const IMPORT_HOST_TEXT_IS_NOT_EMPTY: u32 = 7;
const IMPORT_HOST_COPY_TEXT: u32 = 8;
const IMPORT_HOST_LIST_APPEND_TEXT: u32 = 9;
const IMPORT_HOST_TEXT_MATCHES: u32 = 10;
const IMPORT_HOST_SET_CELL_TEXT_PATTERN: u32 = 11;
const IMPORT_HOST_TEXT_BUILD_START: u32 = 12;
const IMPORT_HOST_TEXT_BUILD_LITERAL: u32 = 13;
const IMPORT_HOST_TEXT_BUILD_CELL: u32 = 14;
const IMPORT_HOST_SET_ITEM_CONTEXT: u32 = 15;
const IMPORT_HOST_CLEAR_ITEM_CONTEXT: u32 = 16;
const IMPORT_HOST_LIST_COPY_ITEM: u32 = 17;
const IMPORT_HOST_LIST_ITEM_MEMORY_INDEX: u32 = 18;
const IMPORT_HOST_LIST_GET_ITEM_F64: u32 = 19;
const IMPORT_HOST_LIST_REPLACE: u32 = 20;
const IMPORT_HOST_GET_CELL_F64: u32 = 21;

const NUM_IMPORTS: u32 = 22;

// Exported function indices (offset by NUM_IMPORTS)
const FN_INIT: u32 = NUM_IMPORTS;
const FN_ON_EVENT: u32 = NUM_IMPORTS + 1;
const FN_SET_GLOBAL: u32 = NUM_IMPORTS + 2;
const FN_INIT_ITEM: u32 = NUM_IMPORTS + 3;
const FN_ON_ITEM_EVENT: u32 = NUM_IMPORTS + 4;
const FN_GET_ITEM_CELL: u32 = NUM_IMPORTS + 5;
const FN_RERUN_RETAIN_FILTERS: u32 = NUM_IMPORTS + 6;

/// Number of base (non-chunk) exported functions.
const NUM_BASE_FUNCTIONS: u32 = 7;

/// Maximum nodes per init chunk function.
/// Chrome enforces a per-function size limit (~7.6MB). With ~120 bytes per node
/// on average, 5000 nodes ≈ 600KB — well under the limit even for heavy nodes.
const INIT_CHUNK_SIZE: usize = 5_000;

/// Maximum cells per set_global br_table. Chrome's V8 enforces a maximum br_table
/// size. We split set_global into sub-functions with at most this many entries each.
const SET_GLOBAL_CHUNK_SIZE: usize = 4_096;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Result of WASM code generation: binary + text patterns for host-side matching.
pub struct WasmOutput {
    pub wasm_bytes: Vec<u8>,
    /// Text patterns used in WHEN/WHILE arms, indexed by pattern_idx.
    pub text_patterns: Vec<String>,
}

pub fn emit_wasm(program: &IrProgram) -> WasmOutput {
    let emitter = WasmEmitter::new(program);
    let wasm_bytes = emitter.emit();
    WasmOutput {
        wasm_bytes,
        text_patterns: emitter.text_patterns.into_inner(),
    }
}

// ---------------------------------------------------------------------------
// Emitter
// ---------------------------------------------------------------------------

/// Info about a ListMap node collected at compile time.
struct ListMapInfo {
    /// The ListMap's own cell ID (used as discriminator in init_item dispatch).
    cell: CellId,
    /// The source list cell that the ListMap reads from.
    source: CellId,
    /// The per-item variable cell (e.g., `todo` in `todos |> List/map(todo => ...)`).
    item_cell: CellId,
    /// Template cell range [start, end).
    cell_range: (u32, u32),
    /// Template event range [start, end).
    event_range: (u32, u32),
}

/// Context for per-item cell access within a ListMap template.
/// Used as a guard to determine which cells are template-scoped.
/// Template cells use WASM globals as workspace during init_item/on_item_event
/// processing (one item at a time), with values persisted to host-side
/// ItemCellStore via host_set_cell_f64.
struct MemoryContext {
    /// WASM local index holding item_idx (i32).
    item_idx_local: u32,
    /// Template cell range start (CellId).
    cell_start: u32,
    /// Template cell range end (CellId, exclusive).
    cell_end: u32,
}

struct WasmEmitter<'a> {
    program: &'a IrProgram,
    /// Text patterns collected during codegen for host-side text matching.
    /// Uses RefCell to allow mutation through &self (avoids borrow conflicts
    /// when iterating over program.nodes while calling emit_* methods).
    text_patterns: RefCell<Vec<String>>,
    /// Local indices for the per-item retain filter loop: (new_list_id, count, i, mem_idx).
    /// Set by emit_init / emit_on_event before emitting code that may trigger
    /// the filter loop via emit_downstream_updates.
    filter_locals: RefCell<Option<(u32, u32, u32, u32)>>,

    // --- Precomputed indices (O(1) lookups instead of O(n) scans) ---
    /// Maps CellId → node index for O(1) find_node_for_cell lookups.
    cell_to_node_idx: HashMap<CellId, usize>,
    /// Maps source CellId → consumer node indices for emit_downstream_updates.
    downstream: HashMap<CellId, Vec<usize>>,
    /// Maps source CellId → consumer node indices for emit_list_downstream_updates.
    list_downstream: HashMap<CellId, Vec<usize>>,
    /// Cells used as source for any list operation (precomputed is_list_source).
    list_source_set: HashSet<CellId>,
}

impl<'a> WasmEmitter<'a> {
    fn new(program: &'a IrProgram) -> Self {
        // Build precomputed indices for O(1) lookups.
        let mut cell_to_node_idx = HashMap::new();
        let mut downstream: HashMap<CellId, Vec<usize>> = HashMap::new();
        let mut list_downstream: HashMap<CellId, Vec<usize>> = HashMap::new();
        let mut list_source_set = HashSet::new();

        for (i, node) in program.nodes.iter().enumerate() {
            // 1. cell_to_node_idx: map each node's output cell to its index.
            if let Some(cell) = Self::node_output_cell(node) {
                cell_to_node_idx.insert(cell, i);
            }

            // 2. downstream: map source cells → consumer node indices.
            //    These are the cells checked by emit_downstream_updates.
            for source in Self::node_downstream_sources(node) {
                downstream.entry(source).or_default().push(i);
            }

            // 3. list_downstream: map list source → consumer node indices.
            //    These are the cells checked by emit_list_downstream_updates.
            if let Some(source) = Self::node_list_source(node) {
                list_downstream.entry(source).or_default().push(i);
                list_source_set.insert(source);
            }
        }

        Self {
            program,
            text_patterns: RefCell::new(Vec::new()),
            filter_locals: RefCell::new(None),
            cell_to_node_idx,
            downstream,
            list_downstream,
            list_source_set,
        }
    }

    /// Extract the output cell from a node (for cell_to_node_idx).
    fn node_output_cell(node: &IrNode) -> Option<CellId> {
        match node {
            IrNode::Derived { cell, .. }
            | IrNode::Hold { cell, .. }
            | IrNode::Latest { target: cell, .. }
            | IrNode::Then { cell, .. }
            | IrNode::When { cell, .. }
            | IrNode::While { cell, .. }
            | IrNode::MathSum { cell, .. }
            | IrNode::PipeThrough { cell, .. }
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
            | IrNode::HoldLoop { cell, .. } => Some(*cell),
            IrNode::Document { .. } | IrNode::Timer { .. } => None,
        }
    }

    /// Extract all source cells that emit_downstream_updates checks for a node.
    fn node_downstream_sources(node: &IrNode) -> Vec<CellId> {
        let mut sources = Vec::new();
        match node {
            IrNode::MathSum { input, .. } => sources.push(*input),
            IrNode::PipeThrough { source, .. } => sources.push(*source),
            IrNode::Derived { expr: IrExpr::CellRead(source), .. } => sources.push(*source),
            IrNode::Derived { expr, .. } if !matches!(expr, IrExpr::CellRead(_)) => {
                Self::collect_expr_cell_refs(expr, &mut sources);
            }
            IrNode::When { source, .. } => sources.push(*source),
            IrNode::While { source, deps, .. } => {
                sources.push(*source);
                sources.extend_from_slice(deps);
            }
            IrNode::Latest { target, arms, .. } => {
                // Non-triggered arms: collect all cells referenced in bodies.
                // The guard is `*target != updated_cell`, so we add all body refs
                // except target itself (target is excluded at match time).
                for arm in arms {
                    if arm.trigger.is_none() {
                        Self::collect_expr_cell_refs(&arm.body, &mut sources);
                    }
                }
                // Remove target from sources (the guard excludes it).
                sources.retain(|c| *c != *target);
            }
            IrNode::ListIsEmpty { source, .. } => sources.push(*source),
            IrNode::TextTrim { source, .. } => sources.push(*source),
            IrNode::TextIsNotEmpty { source, .. } => sources.push(*source),
            IrNode::ListAppend { item, watch_cell, .. } => {
                sources.push(*item);
                if let Some(watch) = watch_cell {
                    // Only add watch_cell if it differs from item to avoid
                    // visiting this node twice for the same cell update.
                    if *watch != *item {
                        sources.push(*watch);
                    }
                }
            }
            IrNode::ListRetain { predicate: Some(pred), .. } => sources.push(*pred),
            IrNode::ListEvery { predicate: Some(pred), .. } => sources.push(*pred),
            IrNode::ListAny { predicate: Some(pred), .. } => sources.push(*pred),
            _ => {}
        }
        sources
    }

    /// Extract the list source cell for emit_list_downstream_updates.
    fn node_list_source(node: &IrNode) -> Option<CellId> {
        match node {
            IrNode::ListCount { source, .. }
            | IrNode::ListMap { source, .. }
            | IrNode::ListIsEmpty { source, .. }
            | IrNode::ListAppend { source, .. }
            | IrNode::ListClear { source, .. }
            | IrNode::ListRemove { source, .. }
            | IrNode::ListRetain { source, .. }
            | IrNode::ListEvery { source, .. }
            | IrNode::ListAny { source, .. } => Some(*source),
            _ => None,
        }
    }

    /// Recursively collect all CellId references from an expression.
    fn collect_expr_cell_refs(expr: &IrExpr, out: &mut Vec<CellId>) {
        match expr {
            IrExpr::CellRead(c) => out.push(*c),
            IrExpr::Compare { lhs, rhs, .. } | IrExpr::BinOp { lhs, rhs, .. } => {
                Self::collect_expr_cell_refs(lhs, out);
                Self::collect_expr_cell_refs(rhs, out);
            }
            IrExpr::UnaryNeg(inner) | IrExpr::Not(inner) => {
                Self::collect_expr_cell_refs(inner, out);
            }
            IrExpr::FieldAccess { object, .. } => {
                Self::collect_expr_cell_refs(object, out);
            }
            IrExpr::TextConcat(segs) => {
                for seg in segs {
                    if let TextSegment::Expr(e) = seg {
                        Self::collect_expr_cell_refs(e, out);
                    }
                }
            }
            IrExpr::PatternMatch { source, arms, .. } => {
                out.push(*source);
                for (_, body) in arms {
                    Self::collect_expr_cell_refs(body, out);
                }
            }
            IrExpr::FunctionCall { args, .. } => {
                for arg in args {
                    Self::collect_expr_cell_refs(arg, out);
                }
            }
            IrExpr::ObjectConstruct(fields) | IrExpr::TaggedObject { fields, .. } => {
                for (_, val) in fields {
                    Self::collect_expr_cell_refs(val, out);
                }
            }
            IrExpr::ListConstruct(items) => {
                for item in items {
                    Self::collect_expr_cell_refs(item, out);
                }
            }
            IrExpr::Constant(_) => {}
        }
    }

    /// Register a text pattern and return its index.
    /// Follow Derived CellRead chains to find the cell that actually holds text.
    /// Used for text pattern matching where an intermediate cell is a pass-through.
    /// Follow Derived CellRead chains to find the underlying cell.
    /// Uses precomputed cell_to_node_idx for O(1) lookups.
    fn resolve_text_cell(&self, cell: CellId) -> CellId {
        let mut current = cell;
        for _ in 0..100 {
            if let Some(node) = self.find_node_for_cell(current) {
                if let IrNode::Derived {
                    expr: IrExpr::CellRead(target),
                    ..
                } = node
                {
                    current = *target;
                    continue;
                }
            }
            break;
        }
        current
    }

    fn register_text_pattern(&self, text: &str) -> u32 {
        let mut patterns = self.text_patterns.borrow_mut();
        if let Some(idx) = patterns.iter().position(|t| t == text) {
            return idx as u32;
        }
        let idx = patterns.len();
        patterns.push(text.to_string());
        idx as u32
    }

    fn emit(&self) -> Vec<u8> {
        let mut module = Module::new();

        // 1. Type section
        let mut types = TypeSection::new();
        // Type 0: (i32, f64) -> () [host_set_cell_f64, host_list_append, set_global]
        types.ty().function([ValType::I32, ValType::F64], []);
        // Type 1: () -> () [host_notify_init_done]
        types.ty().function([], []);
        // Type 2: () -> f64 [host_list_create]
        types.ty().function([], [ValType::F64]);
        // Type 3: (i32, f64) -> () [same as 0]
        types.ty().function([ValType::I32, ValType::F64], []);
        // Type 4: (i32) -> () [host_list_clear]
        types.ty().function([ValType::I32], []);
        // Type 5: (i32) -> f64 [host_list_count, host_text_is_not_empty]
        types.ty().function([ValType::I32], [ValType::F64]);
        // Type 6: () -> () [init]
        types.ty().function([], []);
        // Type 7: (i32) -> () [on_event]
        types.ty().function([ValType::I32], []);
        // Type 8: (i32, i32) -> () [host_text_trim, host_copy_text, host_list_append_text]
        types.ty().function([ValType::I32, ValType::I32], []);
        // Type 9: (i32, i32) -> i32 [host_text_matches]
        types
            .ty()
            .function([ValType::I32, ValType::I32], [ValType::I32]);
        // Type 10: (i32, i32) -> f64 [get_item_cell]
        types
            .ty()
            .function([ValType::I32, ValType::I32], [ValType::F64]);
        // Type 11: (f64, i32, i32) -> () [host_list_copy_item]
        types
            .ty()
            .function([ValType::F64, ValType::I32, ValType::I32], []);
        module.section(&types);

        // 2. Import section
        let mut imports = ImportSection::new();
        imports.import(
            "env",
            "host_set_cell_f64",
            wasm_encoder::EntityType::Function(0),
        );
        imports.import(
            "env",
            "host_notify_init_done",
            wasm_encoder::EntityType::Function(1),
        );
        imports.import(
            "env",
            "host_list_create",
            wasm_encoder::EntityType::Function(2),
        );
        imports.import(
            "env",
            "host_list_append",
            wasm_encoder::EntityType::Function(3),
        );
        imports.import(
            "env",
            "host_list_clear",
            wasm_encoder::EntityType::Function(4),
        );
        imports.import(
            "env",
            "host_list_count",
            wasm_encoder::EntityType::Function(5),
        );
        imports.import(
            "env",
            "host_text_trim",
            wasm_encoder::EntityType::Function(8),
        );
        imports.import(
            "env",
            "host_text_is_not_empty",
            wasm_encoder::EntityType::Function(5),
        );
        imports.import(
            "env",
            "host_copy_text",
            wasm_encoder::EntityType::Function(8),
        );
        imports.import(
            "env",
            "host_list_append_text",
            wasm_encoder::EntityType::Function(8),
        );
        imports.import(
            "env",
            "host_text_matches",
            wasm_encoder::EntityType::Function(9),
        );
        imports.import(
            "env",
            "host_set_cell_text_pattern",
            wasm_encoder::EntityType::Function(8),
        );
        imports.import(
            "env",
            "host_text_build_start",
            wasm_encoder::EntityType::Function(4),
        );
        imports.import(
            "env",
            "host_text_build_literal",
            wasm_encoder::EntityType::Function(4),
        );
        imports.import(
            "env",
            "host_text_build_cell",
            wasm_encoder::EntityType::Function(4),
        );
        imports.import(
            "env",
            "host_set_item_context",
            wasm_encoder::EntityType::Function(4),
        );
        imports.import(
            "env",
            "host_clear_item_context",
            wasm_encoder::EntityType::Function(1),
        );
        imports.import(
            "env",
            "host_list_copy_item",
            wasm_encoder::EntityType::Function(11),
        );
        imports.import(
            "env",
            "host_list_item_memory_index",
            wasm_encoder::EntityType::Function(9),
        );
        imports.import(
            "env",
            "host_list_get_item_f64",
            wasm_encoder::EntityType::Function(10),
        );
        imports.import(
            "env",
            "host_list_replace",
            wasm_encoder::EntityType::Function(8),
        );
        // host_get_cell_f64(cell_id: i32) -> f64
        // Reads per-item cell value from ItemCellStore when item context is active,
        // otherwise reads from global CellStore. Used by filter loops to read
        // per-item field values without WASM linear memory.
        imports.import(
            "env",
            "host_get_cell_f64",
            wasm_encoder::EntityType::Function(5), // Type 5: (i32) -> f64
        );
        module.section(&imports);

        // 3. Function section (declares init, on_event, set_global, init_item, on_item_event, get_item_cell)
        let num_init_chunks = if self.program.nodes.len() > INIT_CHUNK_SIZE {
            (self.program.nodes.len() + INIT_CHUNK_SIZE - 1) / INIT_CHUNK_SIZE
        } else {
            0 // small program — init() handles everything inline
        };
        // Split on_event into per-event handler functions for large programs.
        let split_events = num_init_chunks > 0;
        let num_event_fns = if split_events {
            self.program.events.len()
        } else {
            0
        };
        // Split set_global into sub-functions when too many cells for a single br_table.
        let num_cells = self.program.cells.len();
        let num_set_global_chunks = if num_cells > SET_GLOBAL_CHUNK_SIZE {
            (num_cells + SET_GLOBAL_CHUNK_SIZE - 1) / SET_GLOBAL_CHUNK_SIZE
        } else {
            0
        };
        let mut functions = FunctionSection::new();
        functions.function(6); // init: () -> ()
        functions.function(7); // on_event: (i32) -> ()
        functions.function(0); // set_global: (i32, f64) -> ()
        functions.function(8); // init_item: (i32, i32) -> ()  [item_idx, map_cell]
        functions.function(8); // on_item_event: (i32, i32) -> ()
        functions.function(10); // get_item_cell: (i32, i32) -> f64
        functions.function(6); // rerun_retain_filters: () -> ()
        // Init chunk functions (internal, not exported):
        // Phase 1 chunks (MathSum/Hold init) + Phase 2 chunks (everything else).
        for _ in 0..(2 * num_init_chunks) {
            functions.function(6); // init_chunk_N: () -> ()
        }
        // Per-event handler functions (internal, not exported).
        for _ in 0..num_event_fns {
            functions.function(6); // event_handler_N: () -> ()
        }
        // Per-chunk set_global sub-functions: (i32, f64) -> ()
        for _ in 0..num_set_global_chunks {
            functions.function(0); // set_global_chunk_N: (i32, f64) -> ()
        }
        module.section(&functions);

        // 4. Memory section (1 page for text data)
        let mut memories = MemorySection::new();
        memories.memory(MemoryType {
            minimum: 1,
            maximum: Some(10),
            memory64: false,
            shared: false,
            page_size_log2: None,
        });
        module.section(&memories);

        // 5. Global section — one mutable f64 per cell + 1 temp global
        let mut globals = GlobalSection::new();
        for _cell in &self.program.cells {
            globals.global(
                GlobalType {
                    val_type: ValType::F64,
                    mutable: true,
                    shared: false,
                },
                &ConstExpr::f64_const(0.0),
            );
        }
        // Temp global for intermediate calculations (e.g., list init).
        globals.global(
            GlobalType {
                val_type: ValType::F64,
                mutable: true,
                shared: false,
            },
            &ConstExpr::f64_const(0.0),
        );
        module.section(&globals);

        // 6. Export section
        let mut exports = ExportSection::new();
        exports.export("init", ExportKind::Func, FN_INIT);
        exports.export("on_event", ExportKind::Func, FN_ON_EVENT);
        exports.export("memory", ExportKind::Memory, 0);
        exports.export("set_global", ExportKind::Func, FN_SET_GLOBAL);
        exports.export("init_item", ExportKind::Func, FN_INIT_ITEM);
        exports.export("on_item_event", ExportKind::Func, FN_ON_ITEM_EVENT);
        exports.export("get_item_cell", ExportKind::Func, FN_GET_ITEM_CELL);
        exports.export(
            "rerun_retain_filters",
            ExportKind::Func,
            FN_RERUN_RETAIN_FILTERS,
        );
        module.section(&exports);

        // 7. Code section
        let mut code = CodeSection::new();

        // init() body — Phase 1 inline, Phase 2 either inline or via chunk calls.
        let init_func = self.emit_init(num_init_chunks);
        code.function(&init_func);

        // on_event(event_id) body
        let first_event_fn = NUM_IMPORTS + NUM_BASE_FUNCTIONS + 2 * num_init_chunks as u32;
        let on_event_func = self.emit_on_event(if split_events {
            Some(first_event_fn)
        } else {
            None
        });
        code.function(&on_event_func);

        // set_global(cell_id: i32, value: f64) body
        let first_sg_chunk_fn = NUM_IMPORTS
            + NUM_BASE_FUNCTIONS
            + 2 * num_init_chunks as u32
            + num_event_fns as u32;
        let set_global_func = self.emit_set_global(if num_set_global_chunks > 0 {
            Some((first_sg_chunk_fn, num_set_global_chunks))
        } else {
            None
        });
        code.function(&set_global_func);

        // init_item(item_idx: i32) body
        let init_item_func = self.emit_init_item();
        code.function(&init_item_func);

        // on_item_event(item_idx: i32, event_id: i32) body
        let on_item_event_func = self.emit_on_item_event();
        code.function(&on_item_event_func);

        // get_item_cell(item_idx: i32, cell_offset: i32) -> f64 body
        let get_item_cell_func = self.emit_get_item_cell();
        code.function(&get_item_cell_func);

        // rerun_retain_filters() body
        let rerun_retain_func = self.emit_rerun_retain_filters();
        code.function(&rerun_retain_func);

        // Init chunk functions: Phase 1 chunks first, then Phase 2 chunks.
        for chunk_idx in 0..num_init_chunks {
            let start = chunk_idx * INIT_CHUNK_SIZE;
            let end = ((chunk_idx + 1) * INIT_CHUNK_SIZE).min(self.program.nodes.len());
            let chunk_func = self.emit_init_phase1_chunk(start, end);
            code.function(&chunk_func);
        }
        for chunk_idx in 0..num_init_chunks {
            let start = chunk_idx * INIT_CHUNK_SIZE;
            let end = ((chunk_idx + 1) * INIT_CHUNK_SIZE).min(self.program.nodes.len());
            let chunk_func = self.emit_init_phase2_chunk(start, end);
            code.function(&chunk_func);
        }

        // Per-event handler function bodies.
        for idx in 0..num_event_fns {
            let event_id = EventId(u32::try_from(idx).unwrap());
            let handler_func = self.emit_event_handler_func(event_id);
            code.function(&handler_func);
        }

        // Per-chunk set_global sub-function bodies.
        for chunk_idx in 0..num_set_global_chunks {
            let start = chunk_idx * SET_GLOBAL_CHUNK_SIZE;
            let end = ((chunk_idx + 1) * SET_GLOBAL_CHUNK_SIZE).min(num_cells);
            let chunk_func = self.emit_set_global_chunk(start, end);
            code.function(&chunk_func);
        }

        module.section(&code);

        module.finish()
    }

    /// Emit the `init()` function body.
    /// Check if a cell is used as source for any list operation node.
    /// Uses precomputed list_source_set for O(1) lookup.
    fn is_list_source(&self, cell: CellId) -> bool {
        self.list_source_set.contains(&cell)
    }

    fn emit_init(&self, num_chunks: usize) -> Function {
        if num_chunks > 0 {
            // Large program: init() is a thin dispatcher that calls chunk functions.
            // Phase 1 chunks run first (MathSum/Hold init), then Phase 2 chunks.
            let mut func = Function::new(vec![]);
            let first_chunk_fn = NUM_IMPORTS + NUM_BASE_FUNCTIONS;
            // Call Phase 1 chunks (indices 0..num_chunks).
            for i in 0..num_chunks {
                func.instruction(&Instruction::Call(first_chunk_fn + i as u32));
            }
            // Call Phase 2 chunks (indices num_chunks..2*num_chunks).
            for i in 0..num_chunks {
                func.instruction(&Instruction::Call(
                    first_chunk_fn + num_chunks as u32 + i as u32,
                ));
            }
            func.instruction(&Instruction::Call(IMPORT_HOST_NOTIFY_INIT_DONE));
            func.instruction(&Instruction::End);
            func
        } else {
            // Small program: everything inline in init().
            let mut num_hold_loop_locals: u32 = 0;
            for node in &self.program.nodes {
                if let IrNode::HoldLoop { field_cells, .. } = node {
                    num_hold_loop_locals =
                        num_hold_loop_locals.max(1 + field_cells.len() as u32);
                }
            }
            let has_filter = self.has_per_item_filter();
            let num_f64_locals = num_hold_loop_locals + if has_filter { 1 } else { 0 };
            let mut locals: Vec<(u32, ValType)> = Vec::new();
            if num_f64_locals > 0 {
                locals.push((num_f64_locals, ValType::F64));
            }
            if has_filter {
                locals.push((3, ValType::I32));
            }
            let mut func = Function::new(locals);

            // Phase 1: Initialize MathSum/Hold globals.
            self.emit_init_phase1_nodes(&mut func, &self.program.nodes);

            // Phase 2: Initialize everything else.
            self.emit_init_phase2_setup_filter_locals(num_hold_loop_locals, num_f64_locals);
            self.emit_init_phase2_nodes(&mut func, &self.program.nodes);

            func.instruction(&Instruction::Call(IMPORT_HOST_NOTIFY_INIT_DONE));
            func.instruction(&Instruction::End);
            *self.filter_locals.borrow_mut() = None;
            func
        }
    }

    /// Set up filter_locals for Phase 2 (used by emit_downstream_updates).
    fn emit_init_phase2_setup_filter_locals(
        &self,
        num_hold_loop_locals: u32,
        num_f64_locals: u32,
    ) {
        if self.has_per_item_filter() {
            let local_new_list = num_hold_loop_locals;
            let local_count = num_f64_locals;
            let local_i = num_f64_locals + 1;
            let local_mem_idx = num_f64_locals + 2;
            *self.filter_locals.borrow_mut() =
                Some((local_new_list, local_count, local_i, local_mem_idx));
        }
    }

    /// Emit Phase 1 init code for a slice of nodes (MathSum/Hold only).
    fn emit_init_phase1_nodes(&self, func: &mut Function, nodes: &[IrNode]) {
        for node in nodes {
            match node {
                IrNode::MathSum { cell, .. } => {
                    func.instruction(&Instruction::F64Const(0.0));
                    func.instruction(&Instruction::GlobalSet(cell.0));
                }
                IrNode::Hold { cell, init, .. } => {
                    self.emit_expr(func, init);
                    func.instruction(&Instruction::GlobalSet(cell.0));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    if let IrExpr::CellRead(src) = init {
                        func.instruction(&Instruction::I32Const(cell.0 as i32));
                        func.instruction(&Instruction::I32Const(src.0 as i32));
                        func.instruction(&Instruction::Call(IMPORT_HOST_COPY_TEXT));
                    } else if let Some(text) = self.resolve_expr_text_statically(init) {
                        if !text.is_empty() {
                            let pattern_idx = self.register_text_pattern(&text);
                            func.instruction(&Instruction::I32Const(cell.0 as i32));
                            func.instruction(&Instruction::I32Const(pattern_idx as i32));
                            func.instruction(&Instruction::Call(
                                IMPORT_HOST_SET_CELL_TEXT_PATTERN,
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Emit a Phase 1 chunk function for nodes[start..end].
    fn emit_init_phase1_chunk(&self, start: usize, end: usize) -> Function {
        let mut func = Function::new(vec![]);
        self.emit_init_phase1_nodes(&mut func, &self.program.nodes[start..end]);
        func.instruction(&Instruction::End);
        func
    }

    /// Emit a Phase 2 chunk function for nodes[start..end].
    fn emit_init_phase2_chunk(&self, start: usize, end: usize) -> Function {
        let mut num_hold_loop_locals: u32 = 0;
        for node in &self.program.nodes[start..end] {
            if let IrNode::HoldLoop { field_cells, .. } = node {
                num_hold_loop_locals = num_hold_loop_locals.max(1 + field_cells.len() as u32);
            }
        }
        let has_filter = self.has_per_item_filter();
        let num_f64_locals = num_hold_loop_locals + if has_filter { 1 } else { 0 };
        let mut locals: Vec<(u32, ValType)> = Vec::new();
        if num_f64_locals > 0 {
            locals.push((num_f64_locals, ValType::F64));
        }
        if has_filter {
            locals.push((3, ValType::I32)); // count, i, mem_idx
        }
        let mut func = Function::new(locals);

        self.emit_init_phase2_setup_filter_locals(num_hold_loop_locals, num_f64_locals);
        self.emit_init_phase2_nodes(&mut func, &self.program.nodes[start..end]);

        func.instruction(&Instruction::End);
        *self.filter_locals.borrow_mut() = None;
        func
    }

    /// Emit Phase 2 init code for a slice of nodes.
    fn emit_init_phase2_nodes(&self, func: &mut Function, nodes: &[IrNode]) {
        for node in nodes {
            match node {
                // Skip MathSum and Hold — already initialized in Phase 1.
                IrNode::MathSum { .. } | IrNode::Hold { .. } => {}

                IrNode::HoldLoop {
                    cell: _,
                    field_cells,
                    init_values,
                    count_expr,
                    body_fields,
                } => {
                    // 1. Set field cell globals to initial values.
                    for ((_name, field_cell), (_init_name, init_expr)) in
                        field_cells.iter().zip(init_values.iter())
                    {
                        self.emit_expr(func, init_expr);
                        func.instruction(&Instruction::GlobalSet(field_cell.0));
                    }

                    // 2. Evaluate loop count and store in local 0.
                    self.emit_expr(func, count_expr);
                    func.instruction(&Instruction::LocalSet(0));

                    // 3. WASM loop: iterate count times.
                    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
                    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));

                    // Check: count <= 0 → break
                    func.instruction(&Instruction::LocalGet(0));
                    func.instruction(&Instruction::F64Const(0.0));
                    func.instruction(&Instruction::F64Le);
                    func.instruction(&Instruction::BrIf(1)); // break to outer block

                    // Evaluate each body field expression into temp locals.
                    // All reads see OLD state because we write to temps first.
                    for (i, (_name, body_expr)) in body_fields.iter().enumerate() {
                        self.emit_expr(func, body_expr);
                        func.instruction(&Instruction::LocalSet(1 + i as u32));
                    }

                    // Write temps to globals.
                    for (i, (_name, field_cell)) in field_cells.iter().enumerate() {
                        func.instruction(&Instruction::LocalGet(1 + i as u32));
                        func.instruction(&Instruction::GlobalSet(field_cell.0));
                    }

                    // Decrement counter.
                    func.instruction(&Instruction::LocalGet(0));
                    func.instruction(&Instruction::F64Const(1.0));
                    func.instruction(&Instruction::F64Sub);
                    func.instruction(&Instruction::LocalSet(0));

                    // Continue loop.
                    func.instruction(&Instruction::Br(0)); // back to loop start

                    func.instruction(&Instruction::End); // end loop
                    func.instruction(&Instruction::End); // end block

                    // 4. Notify host of final field cell values.
                    for (_name, field_cell) in field_cells {
                        func.instruction(&Instruction::I32Const(field_cell.0 as i32));
                        func.instruction(&Instruction::GlobalGet(field_cell.0));
                        func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    }
                }

                IrNode::Derived { cell, expr } => {
                    // Check if this is a ListConstruct with items that feeds
                    // into list operations — if so, create a real host list.
                    let is_reactive_list = if let IrExpr::ListConstruct(items) = expr {
                        !items.is_empty() && self.is_list_source(*cell)
                    } else {
                        false
                    };
                    if is_reactive_list {
                        // Create a host-side list (overrides emit_expr's 0.0).
                        func.instruction(&Instruction::Call(IMPORT_HOST_LIST_CREATE));
                    } else {
                        self.emit_expr(func, expr);
                    }
                    func.instruction(&Instruction::GlobalSet(cell.0));
                    // Notify host of initial value.
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    // Set text on the cell if the expression has static text.
                    // Text is stored host-side, not in WASM globals, so it needs
                    // an explicit host call. Without this, text matching (e.g.,
                    // Router/route() |> WHILE { TEXT { / } => ... }) fails because
                    // the cell has empty text during init.
                    if let Some(text) = self.resolve_expr_text_statically(expr) {
                        if !text.is_empty() {
                            let pattern_idx = self.register_text_pattern(&text);
                            func.instruction(&Instruction::I32Const(cell.0 as i32));
                            func.instruction(&Instruction::I32Const(pattern_idx as i32));
                            func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_TEXT_PATTERN));
                        }
                    }
                    // If this is a reactive list with initial items, append them now.
                    if is_reactive_list {
                        if let IrExpr::ListConstruct(items) = expr {
                            for item in items {
                                match item {
                                    IrExpr::CellRead(item_cell)
                                        if self.is_namespace_cell(*item_cell) =>
                                    {
                                        // Namespace cell (object): resolve text from field cells.
                                        let ns_text =
                                            self.resolve_namespace_text_statically(*item_cell);
                                        if let Some(text) = ns_text {
                                            // Set text on the item cell, then append.
                                            let pattern_idx = self.register_text_pattern(&text);
                                            func.instruction(&Instruction::I32Const(
                                                item_cell.0 as i32,
                                            ));
                                            func.instruction(&Instruction::I32Const(
                                                pattern_idx as i32,
                                            ));
                                            func.instruction(&Instruction::Call(
                                                IMPORT_HOST_SET_CELL_TEXT_PATTERN,
                                            ));
                                            func.instruction(&Instruction::I32Const(cell.0 as i32));
                                            func.instruction(&Instruction::I32Const(
                                                item_cell.0 as i32,
                                            ));
                                            func.instruction(&Instruction::Call(
                                                IMPORT_HOST_LIST_APPEND_TEXT,
                                            ));
                                        } else {
                                            // Fallback: append as-is (empty text).
                                            func.instruction(&Instruction::I32Const(cell.0 as i32));
                                            func.instruction(&Instruction::I32Const(
                                                item_cell.0 as i32,
                                            ));
                                            func.instruction(&Instruction::Call(
                                                IMPORT_HOST_LIST_APPEND_TEXT,
                                            ));
                                        }
                                    }
                                    IrExpr::CellRead(item_cell) => {
                                        // host_list_append_text(list_cell_id, item_cell_id)
                                        func.instruction(&Instruction::I32Const(cell.0 as i32));
                                        func.instruction(&Instruction::I32Const(
                                            item_cell.0 as i32,
                                        ));
                                        func.instruction(&Instruction::Call(
                                            IMPORT_HOST_LIST_APPEND_TEXT,
                                        ));
                                    }
                                    IrExpr::TextConcat(segments) => {
                                        // Build text on the list cell, then append from it.
                                        // Use the list cell as a temp text buffer (its text
                                        // doesn't matter since the cell stores a list ID as f64).
                                        // Save list ID: emit_text_build bumps the global,
                                        // which would corrupt the list ID for downstream nodes.
                                        func.instruction(&Instruction::GlobalGet(cell.0));
                                        self.emit_text_build(func, *cell, segments);
                                        // Restore list ID.
                                        func.instruction(&Instruction::GlobalSet(cell.0));
                                        func.instruction(&Instruction::I32Const(cell.0 as i32));
                                        func.instruction(&Instruction::I32Const(cell.0 as i32));
                                        func.instruction(&Instruction::Call(
                                            IMPORT_HOST_LIST_APPEND_TEXT,
                                        ));
                                    }
                                    _ => {
                                        // host_list_append(list_cell_id, value)
                                        func.instruction(&Instruction::I32Const(cell.0 as i32));
                                        self.emit_expr(func, item);
                                        func.instruction(&Instruction::Call(
                                            IMPORT_HOST_LIST_APPEND,
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
                IrNode::Latest { target, arms } => {
                    // Initialize with the first static arm (non-triggered).
                    for arm in arms {
                        if arm.trigger.is_none() {
                            self.emit_expr(func, &arm.body);
                            func.instruction(&Instruction::GlobalSet(target.0));
                            func.instruction(&Instruction::I32Const(target.0 as i32));
                            func.instruction(&Instruction::GlobalGet(target.0));
                            func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                            // Propagate downstream (e.g., to MathSum).
                            self.emit_downstream_updates(func, *target);
                            break;
                        }
                    }
                }
                IrNode::When { cell, source, arms } => {
                    // Evaluate initial value by pattern matching on source cell.
                    self.emit_pattern_match(func, *source, arms, *cell, true);
                }
                IrNode::While {
                    cell, source, arms, ..
                } => {
                    // Same as WHEN for initial evaluation.
                    self.emit_pattern_match(func, *source, arms, *cell, true);
                }
                IrNode::ListAppend { cell, source, .. } => {
                    // Initialize: list cell = source cell (list ID from ListConstruct or
                    // host_list_create). The source creates the list; append just
                    // passes the same list ID through.
                    func.instruction(&Instruction::GlobalGet(source.0));
                    func.instruction(&Instruction::GlobalSet(cell.0));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                }
                IrNode::ListClear { cell, source, .. } => {
                    // Same as append: pass list ID through.
                    func.instruction(&Instruction::GlobalGet(source.0));
                    func.instruction(&Instruction::GlobalSet(cell.0));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                }
                IrNode::ListCount { cell, source } => {
                    // Initialize count from host.
                    func.instruction(&Instruction::I32Const(source.0 as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_LIST_COUNT));
                    func.instruction(&Instruction::GlobalSet(cell.0));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                }
                IrNode::ListMap { cell, source, .. } => {
                    // List map cell just stores the source list ID for bridge rendering.
                    func.instruction(&Instruction::GlobalGet(source.0));
                    func.instruction(&Instruction::GlobalSet(cell.0));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                }
                IrNode::ListRemove { cell, source, .. } => {
                    // At init, nothing has been removed yet. Pass source list through.
                    func.instruction(&Instruction::GlobalGet(source.0));
                    func.instruction(&Instruction::GlobalSet(cell.0));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                }
                IrNode::ListRetain {
                    cell,
                    source,
                    predicate,
                    item_cell,
                    item_field_cells,
                } => {
                    if item_cell.is_some() {
                        // Per-item filtering: run filter loop using saved locals.
                        if let (Some(pred), Some((l0, l1, l2, l3))) =
                            (predicate, *self.filter_locals.borrow())
                        {
                            self.emit_filter_loop(
                                func,
                                *cell,
                                *source,
                                *pred,
                                *item_cell,
                                item_field_cells,
                                l0,
                                l1,
                                l2,
                                l3,
                                false,
                            );
                        }
                    } else if let Some(pred) = predicate {
                        // Binary predicate: truthy → pass source, falsy → empty list.
                        func.instruction(&Instruction::GlobalGet(pred.0));
                        func.instruction(&Instruction::F64Const(0.0));
                        func.instruction(&Instruction::F64Ne);
                        func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
                        func.instruction(&Instruction::GlobalGet(source.0));
                        func.instruction(&Instruction::GlobalSet(cell.0));
                        func.instruction(&Instruction::Else);
                        func.instruction(&Instruction::Call(IMPORT_HOST_LIST_CREATE));
                        func.instruction(&Instruction::GlobalSet(cell.0));
                        func.instruction(&Instruction::End);
                        func.instruction(&Instruction::I32Const(cell.0 as i32));
                        func.instruction(&Instruction::GlobalGet(cell.0));
                        func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    } else {
                        func.instruction(&Instruction::GlobalGet(source.0));
                        func.instruction(&Instruction::GlobalSet(cell.0));
                        func.instruction(&Instruction::I32Const(cell.0 as i32));
                        func.instruction(&Instruction::GlobalGet(cell.0));
                        func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    }
                }
                IrNode::ListEvery {
                    cell,
                    source,
                    predicate,
                    item_cell,
                    item_field_cells,
                }
                | IrNode::ListAny {
                    cell,
                    source,
                    predicate,
                    item_cell,
                    item_field_cells,
                } => {
                    let is_every = matches!(node, IrNode::ListEvery { .. });
                    if item_cell.is_some() {
                        if let (Some(pred), Some((l0, l1, l2, l3))) =
                            (predicate, *self.filter_locals.borrow())
                        {
                            self.emit_boolean_check_loop(
                                func,
                                *cell,
                                *source,
                                *pred,
                                *item_cell,
                                item_field_cells,
                                l0,
                                l1,
                                l2,
                                l3,
                                is_every,
                            );
                        }
                    } else {
                        // No per-item filtering: every([]) = true, any([]) = false.
                        let initial = if is_every { 1.0 } else { 0.0 };
                        func.instruction(&Instruction::F64Const(initial));
                        func.instruction(&Instruction::GlobalSet(cell.0));
                        func.instruction(&Instruction::I32Const(cell.0 as i32));
                        func.instruction(&Instruction::GlobalGet(cell.0));
                        func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    }
                }
                IrNode::ListIsEmpty { cell, source } => {
                    // Check if count == 0 → 1.0 (True), else 0.0 (False).
                    func.instruction(&Instruction::I32Const(source.0 as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_LIST_COUNT));
                    func.instruction(&Instruction::F64Const(0.0));
                    func.instruction(&Instruction::F64Eq);
                    func.instruction(&Instruction::If(wasm_encoder::BlockType::Result(
                        ValType::F64,
                    )));
                    func.instruction(&Instruction::F64Const(1.0));
                    func.instruction(&Instruction::Else);
                    func.instruction(&Instruction::F64Const(0.0));
                    func.instruction(&Instruction::End);
                    func.instruction(&Instruction::GlobalSet(cell.0));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                }
                IrNode::RouterGoTo { cell, source } => {
                    // Initialize as pass-through.
                    func.instruction(&Instruction::GlobalGet(source.0));
                    func.instruction(&Instruction::GlobalSet(cell.0));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                }
                IrNode::TextTrim { cell, source } => {
                    // Call host to trim text_cells[source] → text_cells[cell].
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::I32Const(source.0 as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_TEXT_TRIM));
                    // Pass f64 through for downstream propagation.
                    func.instruction(&Instruction::GlobalGet(source.0));
                    func.instruction(&Instruction::GlobalSet(cell.0));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                }
                IrNode::TextIsNotEmpty { cell, source } => {
                    // Call host to check if text_cells[source] is non-empty.
                    func.instruction(&Instruction::I32Const(source.0 as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_TEXT_IS_NOT_EMPTY));
                    func.instruction(&Instruction::GlobalSet(cell.0));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                }
                IrNode::Document { .. }
                | IrNode::Element { .. }
                | IrNode::Timer { .. }
                | IrNode::Then { .. }
                | IrNode::TextInterpolation { .. }
                | IrNode::PipeThrough { .. }
                | IrNode::CustomCall { .. } => {
                    // These are handled by the bridge, at event time, or at init only.
                }
            }
        }
    }

    /// Emit the `on_event(event_id: i32)` function body.
    /// Uses `br_table` for O(1) dispatch. When `first_event_fn` is Some, dispatches
    /// to per-event handler functions instead of inlining handlers.
    fn emit_on_event(&self, first_event_fn: Option<u32>) -> Function {
        let num_events = self.program.events.len();

        if first_event_fn.is_some() {
            // Large program: dispatch to per-event handler functions.
            let first_fn = first_event_fn.unwrap();
            let mut func = Function::new(vec![]);

            if num_events == 0 {
                func.instruction(&Instruction::End);
                return func;
            }

            // br_table dispatches event_id to the correct handler function call.
            func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
            for _ in 0..num_events {
                func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
            }

            let targets: Vec<u32> = (0..num_events as u32).collect();
            let default_target = num_events as u32;
            func.instruction(&Instruction::LocalGet(0));
            func.instruction(&Instruction::BrTable(targets.into(), default_target));

            for idx in 0..num_events {
                func.instruction(&Instruction::End);
                // Call the per-event handler function.
                func.instruction(&Instruction::Call(first_fn + idx as u32));
                let exit_depth = (num_events - 1 - idx) as u32;
                func.instruction(&Instruction::Br(exit_depth));
            }

            func.instruction(&Instruction::End);
            func.instruction(&Instruction::End);
            func
        } else {
            // Small program: inline all event handlers.
            let has_filter = self.has_per_item_filter();
            let locals: Vec<(u32, ValType)> = if has_filter {
                vec![(1, ValType::F64), (3, ValType::I32)]
            } else {
                vec![]
            };
            if has_filter {
                *self.filter_locals.borrow_mut() = Some((1, 2, 3, 4));
            }
            let mut func = Function::new(locals);

            if num_events == 0 {
                func.instruction(&Instruction::End);
                return func;
            }

            func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
            for _ in 0..num_events {
                func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
            }

            let targets: Vec<u32> = (0..num_events as u32).collect();
            let default_target = num_events as u32;
            func.instruction(&Instruction::LocalGet(0));
            func.instruction(&Instruction::BrTable(targets.into(), default_target));

            for idx in 0..num_events {
                func.instruction(&Instruction::End);
                let event_id = EventId(u32::try_from(idx).unwrap());
                self.emit_event_handler(&mut func, event_id);
                let exit_depth = (num_events - 1 - idx) as u32;
                func.instruction(&Instruction::Br(exit_depth));
            }

            func.instruction(&Instruction::End);
            func.instruction(&Instruction::End);
            *self.filter_locals.borrow_mut() = None;
            func
        }
    }

    /// Emit a standalone function for a single event handler.
    fn emit_event_handler_func(&self, event_id: EventId) -> Function {
        let has_filter = self.has_per_item_filter();
        let locals: Vec<(u32, ValType)> = if has_filter {
            // No param locals (standalone function), so filter locals start at 0.
            vec![(1, ValType::F64), (3, ValType::I32)]
        } else {
            vec![]
        };
        if has_filter {
            // Locals: 0=new_list(f64), 1=count(i32), 2=i(i32), 3=mem_idx(i32)
            *self.filter_locals.borrow_mut() = Some((0, 1, 2, 3));
        }
        let mut func = Function::new(locals);
        self.emit_event_handler(&mut func, event_id);
        func.instruction(&Instruction::End);
        *self.filter_locals.borrow_mut() = None;
        func
    }

    /// Emit the `set_global(cell_id: i32, value: f64)` function body.
    /// When `chunk_info` is Some, dispatches to per-chunk sub-functions to avoid
    /// exceeding Chrome's br_table size limit.
    fn emit_set_global(&self, chunk_info: Option<(u32, usize)>) -> Function {
        let mut func = Function::new([]);
        let num_cells = self.program.cells.len();

        if num_cells == 0 {
            func.instruction(&Instruction::End);
            return func;
        }

        if let Some((first_chunk_fn, num_chunks)) = chunk_info {
            // Two-level dispatch: outer br_table on cell_id / CHUNK_SIZE,
            // then call the appropriate chunk sub-function with (cell_id, value).
            func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
            for _ in 0..num_chunks {
                func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
            }

            // Compute chunk index: cell_id / CHUNK_SIZE
            func.instruction(&Instruction::LocalGet(0));
            func.instruction(&Instruction::I32Const(
                i32::try_from(SET_GLOBAL_CHUNK_SIZE).unwrap(),
            ));
            func.instruction(&Instruction::I32DivU);
            let targets: Vec<u32> = (0..num_chunks as u32).collect();
            let default_target = num_chunks as u32;
            func.instruction(&Instruction::BrTable(targets.into(), default_target));

            for chunk_idx in 0..num_chunks {
                func.instruction(&Instruction::End);
                // Call chunk sub-function with original (cell_id, value).
                func.instruction(&Instruction::LocalGet(0));
                func.instruction(&Instruction::LocalGet(1));
                func.instruction(&Instruction::Call(first_chunk_fn + chunk_idx as u32));
                let exit_depth = (num_chunks - 1 - chunk_idx) as u32;
                func.instruction(&Instruction::Br(exit_depth));
            }

            func.instruction(&Instruction::End);
            func.instruction(&Instruction::End);
        } else {
            // Small program: single-level br_table.
            func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
            for _ in 0..num_cells {
                func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
            }

            let targets: Vec<u32> = (0..num_cells as u32).collect();
            let default_target = num_cells as u32;
            func.instruction(&Instruction::LocalGet(0));
            func.instruction(&Instruction::BrTable(targets.into(), default_target));

            for idx in 0..num_cells {
                func.instruction(&Instruction::End);
                func.instruction(&Instruction::LocalGet(1));
                func.instruction(&Instruction::GlobalSet(u32::try_from(idx).unwrap()));
                let exit_depth = (num_cells - 1 - idx) as u32;
                func.instruction(&Instruction::Br(exit_depth));
            }

            func.instruction(&Instruction::End);
            func.instruction(&Instruction::End);
        }
        func
    }

    /// Emit a set_global chunk sub-function for cells[start..end].
    /// Signature: (cell_id: i32, value: f64) -> ()
    fn emit_set_global_chunk(&self, start: usize, end: usize) -> Function {
        let mut func = Function::new([]);
        let chunk_len = end - start;

        func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
        for _ in 0..chunk_len {
            func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
        }

        // Adjust cell_id to chunk-local index: cell_id - start
        func.instruction(&Instruction::LocalGet(0));
        func.instruction(&Instruction::I32Const(i32::try_from(start).unwrap()));
        func.instruction(&Instruction::I32Sub);
        let targets: Vec<u32> = (0..chunk_len as u32).collect();
        let default_target = chunk_len as u32;
        func.instruction(&Instruction::BrTable(targets.into(), default_target));

        for idx in 0..chunk_len {
            func.instruction(&Instruction::End);
            func.instruction(&Instruction::LocalGet(1));
            func.instruction(&Instruction::GlobalSet(u32::try_from(start + idx).unwrap()));
            let exit_depth = (chunk_len - 1 - idx) as u32;
            func.instruction(&Instruction::Br(exit_depth));
        }

        func.instruction(&Instruction::End);
        func.instruction(&Instruction::End);
        func
    }

    /// Emit handler code for a specific event.
    fn emit_event_handler(&self, func: &mut Function, event_id: EventId) {
        for node in &self.program.nodes {
            match node {
                IrNode::Then {
                    cell,
                    trigger,
                    body,
                } if *trigger == event_id => {
                    if self.is_text_body(body) {
                        // Text body: set text first, then bump counter.
                        self.emit_text_setting(func, *cell, body);
                    } else {
                        // Evaluate body and store to cell.
                        self.emit_expr(func, body);
                        func.instruction(&Instruction::GlobalSet(cell.0));
                    }
                    // Notify host of the cell update.
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    // Propagate to downstream nodes.
                    self.emit_downstream_updates(func, *cell);
                }
                IrNode::Hold {
                    cell,
                    trigger_bodies,
                    ..
                } => {
                    for (trigger, body) in trigger_bodies {
                        if *trigger == event_id {
                            // Evaluate body (which reads current state via GlobalGet).
                            self.emit_expr(func, body);
                            func.instruction(&Instruction::GlobalSet(cell.0));
                            // Notify host.
                            func.instruction(&Instruction::I32Const(cell.0 as i32));
                            func.instruction(&Instruction::GlobalGet(cell.0));
                            func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                            // Update text for boolean-producing bodies so downstream
                            // TextConcat interpolation reads correct "True"/"False".
                            if Self::expr_produces_bool(body) {
                                self.emit_bool_text_update(func, *cell);
                            }
                            // Propagate to downstream nodes (e.g., PipeThrough, MathSum).
                            self.emit_downstream_updates(func, *cell);
                        }
                    }
                }
                IrNode::Latest { target, arms } => {
                    for arm in arms {
                        if arm.trigger == Some(event_id) {
                            let may_skip = matches!(&arm.body, IrExpr::PatternMatch { .. });
                            let text_source = Self::extract_text_source_cell(&arm.body);
                            if self.is_text_body(&arm.body) {
                                // Text body: set text first, then bump counter.
                                self.emit_text_setting(func, *target, &arm.body);
                                // Notify host of the target cell update.
                                func.instruction(&Instruction::I32Const(target.0 as i32));
                                func.instruction(&Instruction::GlobalGet(target.0));
                                func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                                self.emit_downstream_updates(func, *target);
                            } else if may_skip {
                                let skip_global = self.program.cells.len() as u32;
                                self.emit_expr(func, &arm.body);
                                func.instruction(&Instruction::GlobalSet(skip_global));
                                func.instruction(&Instruction::GlobalGet(skip_global));
                                func.instruction(&Instruction::I64ReinterpretF64);
                                func.instruction(&Instruction::I64Const(SKIP_SENTINEL_BITS as i64));
                                func.instruction(&Instruction::I64Ne); // true if NOT skip sentinel
                                func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
                                func.instruction(&Instruction::GlobalGet(skip_global));
                                func.instruction(&Instruction::GlobalSet(target.0));
                                if let Some(src) = text_source {
                                    func.instruction(&Instruction::I32Const(target.0 as i32));
                                    func.instruction(&Instruction::I32Const(src.0 as i32));
                                    func.instruction(&Instruction::Call(IMPORT_HOST_COPY_TEXT));
                                }
                                func.instruction(&Instruction::I32Const(target.0 as i32));
                                func.instruction(&Instruction::GlobalGet(target.0));
                                func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                                self.emit_downstream_updates(func, *target);
                                func.instruction(&Instruction::End);
                            } else {
                                self.emit_expr(func, &arm.body);
                                func.instruction(&Instruction::GlobalSet(target.0));
                                if let Some(src) = text_source {
                                    func.instruction(&Instruction::I32Const(target.0 as i32));
                                    func.instruction(&Instruction::I32Const(src.0 as i32));
                                    func.instruction(&Instruction::Call(IMPORT_HOST_COPY_TEXT));
                                }
                                // Notify host of the target cell update.
                                func.instruction(&Instruction::I32Const(target.0 as i32));
                                func.instruction(&Instruction::GlobalGet(target.0));
                                func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                                self.emit_downstream_updates(func, *target);
                            }
                        }
                    }
                }
                IrNode::ListAppend {
                    cell,
                    source,
                    item,
                    trigger,
                    ..
                } if *trigger == event_id => {
                    if self.is_namespace_cell(*item) {
                        // Namespace cell (object): find the text source and append text.
                        if let Some(text_source) = self.find_text_source_for_namespace(*item) {
                            // Re-evaluate the text source chain to get current text.
                            // Set skip_global = 1.0 (no skip assumed) before re-evaluation.
                            // If the chain includes a WHEN that SKIPs, skip_global will
                            // be left at 0.0, and we won't append.
                            let skip_global = self.program.cells.len() as u32;
                            func.instruction(&Instruction::F64Const(1.0));
                            func.instruction(&Instruction::GlobalSet(skip_global));
                            self.emit_reevaluate_cell(func, text_source);
                            // Check skip flag: if 0.0, a WHEN in the chain SKIPped — don't append.
                            func.instruction(&Instruction::GlobalGet(skip_global));
                            func.instruction(&Instruction::F64Const(0.0));
                            func.instruction(&Instruction::F64Ne);
                            func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
                            // Append text from the text source cell.
                            func.instruction(&Instruction::I32Const(source.0 as i32));
                            func.instruction(&Instruction::I32Const(text_source.0 as i32));
                            func.instruction(&Instruction::Call(IMPORT_HOST_LIST_APPEND_TEXT));
                            self.emit_init_new_item(func, *source);
                            // Update downstream (ListCount, ListMap cells).
                            self.emit_list_downstream_updates(func, *cell);
                            func.instruction(&Instruction::End);
                        } else {
                            // Fallback: append f64 value.
                            func.instruction(&Instruction::I32Const(source.0 as i32));
                            func.instruction(&Instruction::GlobalGet(item.0));
                            func.instruction(&Instruction::Call(IMPORT_HOST_LIST_APPEND));
                            self.emit_init_new_item(func, *source);
                            self.emit_list_downstream_updates(func, *cell);
                        }
                    } else {
                        // Append: call host_list_append(list_cell_id, item_value)
                        func.instruction(&Instruction::I32Const(source.0 as i32));
                        func.instruction(&Instruction::GlobalGet(item.0));
                        func.instruction(&Instruction::Call(IMPORT_HOST_LIST_APPEND));
                        self.emit_init_new_item(func, *source);
                        self.emit_list_downstream_updates(func, *cell);
                    }
                }
                IrNode::ListClear {
                    cell,
                    source,
                    trigger,
                } if *trigger == event_id => {
                    // Clear: call host_list_clear(list_cell_id)
                    func.instruction(&Instruction::I32Const(source.0 as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_LIST_CLEAR));
                    // Update downstream.
                    self.emit_list_downstream_updates(func, *cell);
                }
                IrNode::ListRemove {
                    cell,
                    source,
                    trigger,
                    predicate,
                    item_cell,
                    item_field_cells,
                } if *trigger == event_id => {
                    if let (Some(pred), Some((l0, l1, l2, l3))) =
                        (predicate, *self.filter_locals.borrow())
                    {
                        if !item_field_cells.is_empty() {
                            // Global event + per-item predicate → inverted filter loop.
                            // Creates a temp list with surviving items.
                            self.emit_filter_loop(
                                func,
                                *cell,
                                *source,
                                *pred,
                                *item_cell,
                                item_field_cells,
                                l0,
                                l1,
                                l2,
                                l3,
                                true,
                            );
                            // Copy filtered result back to source list (in-place modification).
                            // This ensures all cells sharing the source list_id see the change,
                            // matching the Actors engine's behavior where ListRemove modifies
                            // the shared list.
                            func.instruction(&Instruction::I32Const(source.0 as i32));
                            func.instruction(&Instruction::I32Const(cell.0 as i32));
                            func.instruction(&Instruction::Call(IMPORT_HOST_LIST_REPLACE));
                            // Reset remove cell to point to same list as source.
                            func.instruction(&Instruction::GlobalGet(source.0));
                            func.instruction(&Instruction::GlobalSet(cell.0));
                            func.instruction(&Instruction::I32Const(cell.0 as i32));
                            func.instruction(&Instruction::GlobalGet(cell.0));
                            func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                        }
                    }
                    self.emit_list_downstream_updates(func, *cell);
                }
                _ => {}
            }
        }

        // Emit downstream updates for payload cells.
        // Payload cells are set by the host BEFORE calling on_event, so their
        // WASM globals already have the new values when we get here.
        let event_info = &self.program.events[event_id.0 as usize];
        for &payload_cell in &event_info.payload_cells {
            self.emit_downstream_updates(func, payload_cell);
        }
    }

    /// Check if a body expression is a text expression (any TextConcat).
    fn is_text_body(&self, body: &IrExpr) -> bool {
        matches!(
            body,
            IrExpr::TextConcat(_) | IrExpr::Constant(IrValue::Text(_))
        )
    }

    /// If `body` is a TextConcat with all-literal segments, emit a call to
    /// `host_set_cell_text_pattern` so the host sets the cell's text content.
    /// Also bumps the cell's f64 global by +1 so signal watchers fire even
    /// when the numeric value would otherwise stay constant (0.0 → 0.0).
    fn emit_text_setting(&self, func: &mut Function, cell: CellId, body: &IrExpr) {
        if let IrExpr::Constant(IrValue::Text(t)) = body {
            // Constant text (e.g., Text/empty(), Text/space()).
            let pattern_idx = self.register_text_pattern(t);
            func.instruction(&Instruction::I32Const(cell.0 as i32));
            func.instruction(&Instruction::I32Const(pattern_idx as i32));
            func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_TEXT_PATTERN));
            // Bump f64 global so signal fires.
            func.instruction(&Instruction::GlobalGet(cell.0));
            func.instruction(&Instruction::F64Const(1.0));
            func.instruction(&Instruction::F64Add);
            func.instruction(&Instruction::GlobalSet(cell.0));
        } else if let IrExpr::TextConcat(segments) = body {
            // Collect all-literal text.
            let mut all_literal = true;
            let mut text = String::new();
            for seg in segments {
                match seg {
                    TextSegment::Literal(s) => text.push_str(s),
                    _ => {
                        all_literal = false;
                        break;
                    }
                }
            }
            if all_literal {
                let pattern_idx = self.register_text_pattern(&text);
                func.instruction(&Instruction::I32Const(cell.0 as i32));
                func.instruction(&Instruction::I32Const(pattern_idx as i32));
                func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_TEXT_PATTERN));
                // Bump f64 global so signal fires on repeated text assignments.
                func.instruction(&Instruction::GlobalGet(cell.0));
                func.instruction(&Instruction::F64Const(1.0));
                func.instruction(&Instruction::F64Add);
                func.instruction(&Instruction::GlobalSet(cell.0));
            } else {
                // Reactive TextConcat: build text on the host side segment by segment.
                self.emit_text_build(func, cell, segments);
            }
        } else if let IrExpr::CellRead(source) = body {
            // Copy text from the source cell to this cell.
            func.instruction(&Instruction::I32Const(cell.0 as i32));
            func.instruction(&Instruction::I32Const(source.0 as i32));
            func.instruction(&Instruction::Call(IMPORT_HOST_COPY_TEXT));
        }
    }

    /// Emit host calls to build a reactive TextConcat on the host side.
    /// Uses host_text_build_start/literal/cell to assemble text from mixed segments.
    fn emit_text_build(&self, func: &mut Function, target: CellId, segments: &[TextSegment]) {
        func.instruction(&Instruction::I32Const(target.0 as i32));
        func.instruction(&Instruction::Call(IMPORT_HOST_TEXT_BUILD_START));
        for seg in segments {
            match seg {
                TextSegment::Literal(s) => {
                    let pattern_idx = self.register_text_pattern(s);
                    func.instruction(&Instruction::I32Const(pattern_idx as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_TEXT_BUILD_LITERAL));
                }
                TextSegment::Expr(IrExpr::CellRead(cell)) => {
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_TEXT_BUILD_CELL));
                }
                TextSegment::Expr(_) => {
                    // Non-cell expressions: emit as literal "?" placeholder.
                    let pattern_idx = self.register_text_pattern("?");
                    func.instruction(&Instruction::I32Const(pattern_idx as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_TEXT_BUILD_LITERAL));
                }
            }
        }
        // Bump f64 global so signal fires after text is built.
        func.instruction(&Instruction::GlobalGet(target.0));
        func.instruction(&Instruction::F64Const(1.0));
        func.instruction(&Instruction::F64Add);
        func.instruction(&Instruction::GlobalSet(target.0));
    }

    /// Emit a pattern-match block: compare source cell value against patterns,
    /// execute the matching arm's body, store to target cell, notify host.
    fn emit_pattern_match(
        &self,
        func: &mut Function,
        source: CellId,
        arms: &[(IrPattern, IrExpr)],
        target: CellId,
        propagate_downstream: bool,
    ) {
        // Emit nested if-else chain so only the FIRST matching arm executes.
        // Without this, wildcards would always overwrite earlier matches.
        self.emit_pattern_arms(func, source, arms, target, 0, propagate_downstream);
    }

    /// Recursively emit pattern match arms as nested if-else blocks.
    fn emit_pattern_arms(
        &self,
        func: &mut Function,
        source: CellId,
        arms: &[(IrPattern, IrExpr)],
        target: CellId,
        idx: usize,
        propagate_downstream: bool,
    ) {
        if idx >= arms.len() {
            return;
        }

        let (pattern, body) = &arms[idx];
        let is_skip = matches!(body, IrExpr::Constant(IrValue::Skip));
        let has_more = idx + 1 < arms.len();

        match pattern {
            IrPattern::Tag(tag) => {
                let encoded = self
                    .program
                    .tag_table
                    .iter()
                    .position(|t| t == tag)
                    .map(|i| (i + 1) as f64)
                    .unwrap_or(0.0);
                func.instruction(&Instruction::GlobalGet(source.0));
                func.instruction(&Instruction::F64Const(encoded));
                func.instruction(&Instruction::F64Eq);
                func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
                if !is_skip {
                    self.emit_arm_body(func, body, target, propagate_downstream);
                }
                if has_more {
                    func.instruction(&Instruction::Else);
                    self.emit_pattern_arms(func, source, arms, target, idx + 1, propagate_downstream);
                }
                func.instruction(&Instruction::End);
            }
            IrPattern::Number(n) => {
                func.instruction(&Instruction::GlobalGet(source.0));
                func.instruction(&Instruction::F64Const(*n));
                func.instruction(&Instruction::F64Eq);
                func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
                if !is_skip {
                    self.emit_arm_body(func, body, target, propagate_downstream);
                }
                if has_more {
                    func.instruction(&Instruction::Else);
                    self.emit_pattern_arms(func, source, arms, target, idx + 1, propagate_downstream);
                }
                func.instruction(&Instruction::End);
            }
            IrPattern::Text(text) => {
                let pattern_idx = self.register_text_pattern(text);
                let text_source = self.resolve_text_cell(source);
                func.instruction(&Instruction::I32Const(text_source.0 as i32));
                func.instruction(&Instruction::I32Const(pattern_idx as i32));
                func.instruction(&Instruction::Call(IMPORT_HOST_TEXT_MATCHES));
                func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
                if !is_skip {
                    self.emit_arm_body(func, body, target, propagate_downstream);
                }
                if has_more {
                    func.instruction(&Instruction::Else);
                    self.emit_pattern_arms(func, source, arms, target, idx + 1, propagate_downstream);
                }
                func.instruction(&Instruction::End);
            }
            IrPattern::Wildcard | IrPattern::Binding(_) => {
                // Wildcard matches everything — no remaining arms matter.
                if !is_skip {
                    self.emit_arm_body(func, body, target, propagate_downstream);
                }
            }
        }
    }

    /// Emit a WHEN/WHILE arm body: evaluate expression, set target cell, copy text if
    /// the body reads from another cell, and propagate downstream.
    ///
    /// If the body's dependency chain includes a WHEN that SKIPped, the entire arm body
    /// is skipped (no cell update, no downstream propagation). This implements nested
    /// SKIP propagation: inner WHEN SKIP → outer arm body also skips.
    fn emit_arm_body(&self, func: &mut Function, body: &IrExpr, target: CellId, propagate_downstream: bool) {
        let skip_global = self.program.cells.len() as u32;

        // Set skip flag = 1.0 (no skip) before re-evaluating the dependency chain.
        // If the chain includes a WHEN that SKIPs, emit_reevaluate_cell will leave
        // skip_global at 0.0.
        func.instruction(&Instruction::F64Const(1.0));
        func.instruction(&Instruction::GlobalSet(skip_global));

        // Before evaluating the body, re-evaluate any block-local dependency chain.
        // If the body is CellRead(cell), walk up the node graph for that cell
        // and re-evaluate TextTrim/TextIsNotEmpty/Derived(CellRead) nodes.
        self.emit_reevaluate_chain(func, body);

        // Check skip flag: if 0.0, an upstream WHEN in the chain SKIPped — don't
        // update the target cell or propagate downstream.
        func.instruction(&Instruction::GlobalGet(skip_global));
        func.instruction(&Instruction::F64Const(0.0));
        func.instruction(&Instruction::F64Ne);
        func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));

        if self.is_text_body(body) {
            // Text body: build text and bump f64 BEFORE notifying host,
            // so the signal fires with the bumped value.
            self.emit_text_setting(func, target, body);
        } else {
            self.emit_expr(func, body);
            func.instruction(&Instruction::GlobalSet(target.0));
            // Copy text from source cell if body is CellRead.
            if let IrExpr::CellRead(src) = body {
                func.instruction(&Instruction::I32Const(target.0 as i32));
                func.instruction(&Instruction::I32Const(src.0 as i32));
                func.instruction(&Instruction::Call(IMPORT_HOST_COPY_TEXT));
            } else if let Some(text) = self.resolve_expr_text_statically(body) {
                // Set text for constant expressions (tags, text literals).
                if !text.is_empty() {
                    let pattern_idx = self.register_text_pattern(&text);
                    func.instruction(&Instruction::I32Const(target.0 as i32));
                    func.instruction(&Instruction::I32Const(pattern_idx as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_TEXT_PATTERN));
                }
            }
        }
        // Notify host of the cell update.
        func.instruction(&Instruction::I32Const(target.0 as i32));
        func.instruction(&Instruction::GlobalGet(target.0));
        func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
        if propagate_downstream {
            self.emit_downstream_updates(func, target);
        }

        // Signal that this arm produced a value (for callers checking skip_global).
        func.instruction(&Instruction::F64Const(1.0));
        func.instruction(&Instruction::GlobalSet(skip_global));

        func.instruction(&Instruction::End); // end skip check if-block
    }

    /// Re-evaluate the dependency chain for a CellRead expression.
    /// Walks up through Derived(CellRead), TextTrim, TextIsNotEmpty, and WHEN nodes
    /// to ensure block-local cells are fresh before reading.
    fn emit_reevaluate_chain(&self, func: &mut Function, expr: &IrExpr) {
        if let IrExpr::CellRead(cell) = expr {
            self.emit_reevaluate_cell(func, *cell);
        }
    }

    /// Re-evaluate a single cell and its upstream dependencies.
    fn emit_reevaluate_cell(&self, func: &mut Function, cell: CellId) {
        let mut visiting = HashSet::new();
        self.emit_reevaluate_cell_guarded(func, cell, &mut visiting);
    }

    fn emit_reevaluate_cell_guarded(
        &self,
        func: &mut Function,
        cell: CellId,
        visiting: &mut HashSet<CellId>,
    ) {
        if !visiting.insert(cell) {
            // Dependency graph can include cycles (e.g., HOLD state references).
            // Stop at the cycle edge to avoid unbounded recursive codegen.
            return;
        }
        if let Some(node) = self.find_node_for_cell(cell) {
            match node {
                IrNode::Derived {
                    expr: IrExpr::CellRead(source),
                    ..
                } => {
                    // Re-evaluate upstream first.
                    self.emit_reevaluate_cell_guarded(func, *source, visiting);
                    // Copy value from source.
                    func.instruction(&Instruction::GlobalGet(source.0));
                    func.instruction(&Instruction::GlobalSet(cell.0));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    // Copy text too.
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::I32Const(source.0 as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_COPY_TEXT));
                }
                IrNode::TextTrim { source, .. } => {
                    self.emit_reevaluate_cell_guarded(func, *source, visiting);
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::I32Const(source.0 as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_TEXT_TRIM));
                    func.instruction(&Instruction::GlobalGet(source.0));
                    func.instruction(&Instruction::GlobalSet(cell.0));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                }
                IrNode::TextIsNotEmpty { source, .. } => {
                    self.emit_reevaluate_cell_guarded(func, *source, visiting);
                    func.instruction(&Instruction::I32Const(source.0 as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_TEXT_IS_NOT_EMPTY));
                    func.instruction(&Instruction::GlobalSet(cell.0));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                }
                IrNode::When { source, arms, .. } => {
                    self.emit_reevaluate_cell_guarded(func, *source, visiting);
                    // Set skip flag = 0.0 before re-evaluation.
                    // emit_pattern_arms will set it to 1.0 if a non-SKIP arm executes.
                    let skip_global = self.program.cells.len() as u32;
                    func.instruction(&Instruction::F64Const(0.0));
                    func.instruction(&Instruction::GlobalSet(skip_global));
                    // Re-evaluate the pattern match inline. Don't propagate downstream —
                    // this is an upstream re-evaluation context (refreshing stale cells),
                    // not a change propagation. Propagating would cycle through While deps.
                    self.emit_pattern_match(func, *source, arms, cell, false);
                }
                IrNode::PipeThrough { source, .. } => {
                    self.emit_reevaluate_cell_guarded(func, *source, visiting);
                    func.instruction(&Instruction::GlobalGet(source.0));
                    func.instruction(&Instruction::GlobalSet(cell.0));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::I32Const(source.0 as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_COPY_TEXT));
                }
                _ => {
                    // Other node types: don't need re-evaluation (constants, elements, etc.)
                }
            }
        }
        visiting.remove(&cell);
    }

    /// Re-evaluate the dependency chain for a CellRead expression (per-item version).
    /// Uses memory context to read/write per-item cells from WASM linear memory.
    fn emit_reevaluate_chain_ctx(
        &self,
        func: &mut Function,
        expr: &IrExpr,
        mem_ctx: &MemoryContext,
    ) {
        if let IrExpr::CellRead(cell) = expr {
            self.emit_reevaluate_cell_ctx(func, *cell, mem_ctx);
        }
    }

    /// Re-evaluate a single cell and its upstream dependencies (per-item version).
    fn emit_reevaluate_cell_ctx(&self, func: &mut Function, cell: CellId, mem_ctx: &MemoryContext) {
        let mut visiting = HashSet::new();
        self.emit_reevaluate_cell_ctx_guarded(func, cell, mem_ctx, &mut visiting);
    }

    fn emit_reevaluate_cell_ctx_guarded(
        &self,
        func: &mut Function,
        cell: CellId,
        mem_ctx: &MemoryContext,
        visiting: &mut HashSet<CellId>,
    ) {
        if !visiting.insert(cell) {
            // Template cell graph can also contain cycles; stop recursion at cycle edge.
            return;
        }
        if let Some(node) = self.find_node_for_cell(cell) {
            match node {
                IrNode::Derived {
                    expr: IrExpr::CellRead(source),
                    ..
                } => {
                    self.emit_reevaluate_cell_ctx_guarded(func, *source, mem_ctx, visiting);
                    self.emit_cell_get(func, *source, Some(mem_ctx));
                    self.emit_cell_set(func, cell, Some(mem_ctx));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    self.emit_cell_get(func, cell, Some(mem_ctx));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::I32Const(source.0 as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_COPY_TEXT));
                }
                IrNode::TextTrim { source, .. } => {
                    self.emit_reevaluate_cell_ctx_guarded(func, *source, mem_ctx, visiting);
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::I32Const(source.0 as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_TEXT_TRIM));
                    self.emit_cell_get(func, *source, Some(mem_ctx));
                    self.emit_cell_set(func, cell, Some(mem_ctx));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    self.emit_cell_get(func, cell, Some(mem_ctx));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                }
                IrNode::TextIsNotEmpty { source, .. } => {
                    self.emit_reevaluate_cell_ctx_guarded(func, *source, mem_ctx, visiting);
                    func.instruction(&Instruction::I32Const(source.0 as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_TEXT_IS_NOT_EMPTY));
                    self.emit_cell_set(func, cell, Some(mem_ctx));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    self.emit_cell_get(func, cell, Some(mem_ctx));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                }
                IrNode::When { source, arms, .. } => {
                    self.emit_reevaluate_cell_ctx_guarded(func, *source, mem_ctx, visiting);
                    // Also re-evaluate any CellRead arm bodies that have their own
                    // dependency chains (e.g., inner WHEN depending on TextIsNotEmpty).
                    // Without this, nested chains are stale when the pattern match reads them.
                    for (_, arm_body) in arms {
                        if let IrExpr::CellRead(arm_cell) = arm_body {
                            self.emit_reevaluate_cell_ctx_guarded(
                                func, *arm_cell, mem_ctx, visiting,
                            );
                        }
                    }
                    // Don't propagate downstream — upstream re-evaluation context only.
                    self.emit_pattern_match_ctx(func, *source, arms, cell, Some(mem_ctx), false);
                }
                IrNode::PipeThrough { source, .. } => {
                    self.emit_reevaluate_cell_ctx_guarded(func, *source, mem_ctx, visiting);
                    self.emit_cell_get(func, *source, Some(mem_ctx));
                    self.emit_cell_set(func, cell, Some(mem_ctx));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    self.emit_cell_get(func, cell, Some(mem_ctx));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::I32Const(source.0 as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_COPY_TEXT));
                }
                _ => {
                    // Other node types (Derived with Void, Element, etc.): don't need re-evaluation.
                }
            }
        }
        visiting.remove(&cell);
    }

    /// Find the IrNode that defines a cell (for re-evaluation).
    /// Uses precomputed cell_to_node_idx for O(1) lookup.
    fn find_node_for_cell(&self, cell: CellId) -> Option<&IrNode> {
        self.cell_to_node_idx
            .get(&cell)
            .map(|&idx| &self.program.nodes[idx])
    }

    /// Resolve the display text for a cell statically by following CellRead chains
    /// through Derived and HOLD nodes to find literal text (TextConcat with all literals,
    /// Constant(Text), or Constant(Tag)). Returns None if text can't be resolved statically.
    fn resolve_cell_text_statically(&self, cell: CellId) -> Option<String> {
        self.resolve_cell_text_statically_depth(cell, 0)
    }

    fn resolve_cell_text_statically_depth(&self, cell: CellId, depth: u32) -> Option<String> {
        if depth > 20 {
            return None;
        }
        let node = self.find_node_for_cell(cell)?;
        match node {
            IrNode::Hold { init, .. } => self.resolve_expr_text_statically_depth(init, depth + 1),
            IrNode::Latest { arms, .. } => {
                // Prefer non-triggered arms for initial static text resolution.
                // Event-driven arms are not available before events fire.
                for arm in arms {
                    if arm.trigger.is_none() {
                        if let Some(text) =
                            self.resolve_expr_text_statically_depth(&arm.body, depth + 1)
                        {
                            return Some(text);
                        }
                    }
                }
                // Fallback: try all arms in declaration order.
                for arm in arms {
                    if let Some(text) =
                        self.resolve_expr_text_statically_depth(&arm.body, depth + 1)
                    {
                        return Some(text);
                    }
                }
                None
            }
            IrNode::Derived { expr, .. } => {
                self.resolve_expr_text_statically_depth(expr, depth + 1)
            }
            IrNode::TextInterpolation { parts, .. } => {
                let mut text = String::new();
                for seg in parts {
                    match seg {
                        TextSegment::Literal(s) => text.push_str(s),
                        _ => return None,
                    }
                }
                Some(text)
            }
            _ => None,
        }
    }

    /// Resolve an expression to literal text by following CellRead chains.
    fn resolve_expr_text_statically(&self, expr: &IrExpr) -> Option<String> {
        self.resolve_expr_text_statically_depth(expr, 0)
    }

    fn resolve_expr_text_statically_depth(&self, expr: &IrExpr, depth: u32) -> Option<String> {
        if depth > 20 {
            return None;
        }
        match expr {
            IrExpr::TextConcat(segs) => {
                let mut text = String::new();
                for seg in segs {
                    match seg {
                        TextSegment::Literal(s) => text.push_str(s),
                        TextSegment::Expr(_) => return None,
                    }
                }
                Some(text)
            }
            IrExpr::CellRead(cell) => self.resolve_cell_text_statically_depth(*cell, depth + 1),
            IrExpr::Constant(IrValue::Text(t)) => Some(t.clone()),
            IrExpr::Constant(IrValue::Tag(t)) => Some(t.clone()),
            IrExpr::Constant(IrValue::Bool(b)) => {
                Some(if *b { "True" } else { "False" }.to_string())
            }
            _ => None,
        }
    }

    /// Resolve text only when it is structurally literal/stable for comparisons.
    /// Unlike `resolve_expr_text_statically*`, this intentionally does NOT follow
    /// dynamic nodes like Latest/Hold, because those can change after init.
    fn resolve_expr_text_literal_for_compare(&self, expr: &IrExpr, depth: u32) -> Option<String> {
        if depth > 20 {
            return None;
        }
        match expr {
            IrExpr::Constant(IrValue::Text(t)) => Some(t.clone()),
            IrExpr::TextConcat(segs) => {
                let mut text = String::new();
                for seg in segs {
                    match seg {
                        TextSegment::Literal(s) => text.push_str(s),
                        TextSegment::Expr(_) => return None,
                    }
                }
                Some(text)
            }
            IrExpr::CellRead(cell) => self.resolve_cell_text_literal_for_compare(*cell, depth + 1),
            _ => None,
        }
    }

    fn resolve_cell_text_literal_for_compare(&self, cell: CellId, depth: u32) -> Option<String> {
        if depth > 20 {
            return None;
        }
        let node = self.find_node_for_cell(cell)?;
        match node {
            IrNode::Derived { expr, .. } => {
                self.resolve_expr_text_literal_for_compare(expr, depth + 1)
            }
            IrNode::Hold {
                init,
                trigger_bodies,
                ..
            } if trigger_bodies.is_empty() => {
                self.resolve_expr_text_literal_for_compare(init, depth + 1)
            }
            IrNode::Latest { arms, .. } if arms.iter().all(|arm| arm.trigger.is_none()) => {
                for arm in arms {
                    if let Some(text) =
                        self.resolve_expr_text_literal_for_compare(&arm.body, depth + 1)
                    {
                        return Some(text);
                    }
                }
                None
            }
            IrNode::PipeThrough { source, .. } => {
                self.resolve_cell_text_literal_for_compare(*source, depth + 1)
            }
            IrNode::TextInterpolation { parts, .. } => {
                let mut text = String::new();
                for seg in parts {
                    match seg {
                        TextSegment::Literal(s) => text.push_str(s),
                        _ => return None,
                    }
                }
                Some(text)
            }
            _ => None,
        }
    }

    /// For a namespace cell (object), find the primary text-bearing field cell
    /// and resolve its text statically. Prefers real text over Bool values
    /// because cell_field_cells is a HashMap with non-deterministic iteration
    /// order — without preference, Bool fields like `editing: False` can be
    /// returned instead of the actual title field.
    fn resolve_namespace_text_statically(&self, cell: CellId) -> Option<String> {
        if let Some(fields) = self.program.cell_field_cells.get(&cell) {
            let mut bool_fallback: Option<String> = None;
            for (_name, field_cell) in fields.iter() {
                if let Some(text) = self.resolve_cell_text_statically(*field_cell) {
                    if !text.is_empty() {
                        if text != "True" && text != "False" {
                            return Some(text);
                        } else if bool_fallback.is_none() {
                            bool_fallback = Some(text);
                        }
                    }
                }
            }
            return bool_fallback;
        }
        None
    }

    /// Check if a cell is a namespace cell (Void constant).
    fn is_namespace_cell(&self, cell: CellId) -> bool {
        self.program.cell_field_cells.contains_key(&cell)
    }

    /// For a namespace cell (object), find the runtime text source cell.
    /// Follows field cells to find a HOLD with CellRead init (returns the source)
    /// or a Derived with text (returns the field cell itself).
    /// Prefers sources that resolve to real text over Bool values (same HashMap
    /// iteration order issue as resolve_namespace_text_statically).
    fn find_text_source_for_namespace(&self, cell: CellId) -> Option<CellId> {
        let fields = self.program.cell_field_cells.get(&cell)?;
        let mut fallback: Option<CellId> = None;
        for (_name, field_cell) in fields.iter() {
            if let Some(node) = self.find_node_for_cell(*field_cell) {
                let candidate = match node {
                    IrNode::Hold { init, .. } => {
                        if let IrExpr::CellRead(source) = init {
                            Some(*source)
                        } else {
                            None
                        }
                    }
                    IrNode::Derived {
                        expr: IrExpr::CellRead(source),
                        ..
                    } => Some(*source),
                    IrNode::Derived {
                        expr: IrExpr::TextConcat(_),
                        ..
                    } => {
                        return Some(*field_cell); // TextConcat is always real text
                    }
                    _ => None,
                };
                if let Some(source) = candidate {
                    // Check if source resolves to non-Bool text.
                    if let Some(text) = self.resolve_cell_text_statically(source) {
                        if text != "True" && text != "False" {
                            return Some(source);
                        }
                    }
                    if fallback.is_none() {
                        fallback = Some(source);
                    }
                }
            }
        }
        fallback
    }

    /// After a ListRemove filter, propagate the new filtered list ID from
    /// `filtered_cell` back up through the chain to the root ListConstruct.
    /// This ensures subsequent ListAppend operations append to the filtered list.
    fn emit_list_chain_propagate(
        &self,
        func: &mut Function,
        source: CellId,
        filtered_cell: CellId,
    ) {
        // Walk up the chain from source to root, collecting cells to update.
        let mut cells_to_update = Vec::new();
        let mut current = source;
        loop {
            cells_to_update.push(current);
            // Find the node that defines `current` and get its source.
            let parent = self.program.nodes.iter().find_map(|node| match node {
                IrNode::ListAppend { cell, source, .. } if *cell == current => Some(*source),
                IrNode::ListRemove { cell, source, .. } if *cell == current => Some(*source),
                IrNode::ListClear { cell, source, .. } if *cell == current => Some(*source),
                IrNode::ListRetain { cell, source, .. } if *cell == current => Some(*source),
                _ => None,
            });
            match parent {
                Some(p) => current = p,
                None => break, // Reached ListConstruct or root
            }
        }
        // Set each upstream cell's global and host value to the filtered list ID.
        for cell in cells_to_update {
            func.instruction(&Instruction::GlobalGet(filtered_cell.0));
            func.instruction(&Instruction::GlobalSet(cell.0));
            func.instruction(&Instruction::I32Const(cell.0 as i32));
            func.instruction(&Instruction::GlobalGet(cell.0));
            func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
        }
    }

    /// After a list mutation (append/clear), update downstream ListCount and ListMap cells.
    /// Uses precomputed `list_downstream` index for O(1) consumer lookup.
    fn emit_list_downstream_updates(&self, func: &mut Function, list_cell: CellId) {
        let empty = Vec::new();
        let consumer_indices = self.list_downstream.get(&list_cell).unwrap_or(&empty);
        for &idx in consumer_indices {
            let node = &self.program.nodes[idx];
            match node {
                IrNode::ListCount { cell, source } if *source == list_cell => {
                    // Re-read count from host.
                    func.instruction(&Instruction::I32Const(source.0 as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_LIST_COUNT));
                    func.instruction(&Instruction::GlobalSet(cell.0));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    // Propagate count updates further downstream.
                    self.emit_downstream_updates(func, *cell);
                }
                IrNode::ListMap { cell, source, .. } if *source == list_cell => {
                    // Notify host that list map needs re-render.
                    // We bump the cell value (version) so signals trigger.
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::F64Const(1.0));
                    func.instruction(&Instruction::F64Add);
                    func.instruction(&Instruction::GlobalSet(cell.0));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                }
                // ListIsEmpty downstream of list mutation.
                IrNode::ListIsEmpty { cell, source } if *source == list_cell => {
                    func.instruction(&Instruction::I32Const(source.0 as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_LIST_COUNT));
                    func.instruction(&Instruction::F64Const(0.0));
                    func.instruction(&Instruction::F64Eq);
                    func.instruction(&Instruction::If(wasm_encoder::BlockType::Result(
                        ValType::F64,
                    )));
                    func.instruction(&Instruction::F64Const(1.0));
                    func.instruction(&Instruction::Else);
                    func.instruction(&Instruction::F64Const(0.0));
                    func.instruction(&Instruction::End);
                    func.instruction(&Instruction::GlobalSet(cell.0));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    self.emit_downstream_updates(func, *cell);
                }
                // ListAppend/ListClear/ListRemove/ListRetain chain:
                // When traversed as downstream (not their own trigger), copy source → cell.
                IrNode::ListAppend { cell, source, .. } if *source == list_cell => {
                    func.instruction(&Instruction::GlobalGet(source.0));
                    func.instruction(&Instruction::GlobalSet(cell.0));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    self.emit_list_downstream_updates(func, *cell);
                }
                IrNode::ListClear { cell, source, .. } if *source == list_cell => {
                    func.instruction(&Instruction::GlobalGet(source.0));
                    func.instruction(&Instruction::GlobalSet(cell.0));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    self.emit_list_downstream_updates(func, *cell);
                }
                IrNode::ListRemove { cell, source, .. } if *source == list_cell => {
                    // Copy source list_id to this cell so downstream sees the updated list.
                    func.instruction(&Instruction::GlobalGet(source.0));
                    func.instruction(&Instruction::GlobalSet(cell.0));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    self.emit_list_downstream_updates(func, *cell);
                }
                IrNode::ListRetain {
                    cell,
                    source,
                    predicate,
                    item_cell,
                    item_field_cells,
                } if *source == list_cell => {
                    if let (Some(pred), Some((l0, l1, l2, l3))) =
                        (predicate, *self.filter_locals.borrow())
                    {
                        if item_cell.is_some() {
                            // Per-item filtering: re-run filter loop when source list changes.
                            self.emit_filter_loop(
                                func,
                                *cell,
                                *source,
                                *pred,
                                *item_cell,
                                item_field_cells,
                                l0,
                                l1,
                                l2,
                                l3,
                                false,
                            );
                        }
                    }
                    self.emit_list_downstream_updates(func, *cell);
                }
                IrNode::ListEvery {
                    cell,
                    source,
                    predicate,
                    item_cell,
                    item_field_cells,
                } if *source == list_cell => {
                    if let (Some(pred), Some((l0, l1, l2, l3))) =
                        (predicate, *self.filter_locals.borrow())
                    {
                        if item_cell.is_some() {
                            self.emit_boolean_check_loop(
                                func, *cell, *source, *pred, *item_cell,
                                item_field_cells, l0, l1, l2, l3, true,
                            );
                        }
                    }
                    self.emit_downstream_updates(func, *cell);
                }
                IrNode::ListAny {
                    cell,
                    source,
                    predicate,
                    item_cell,
                    item_field_cells,
                } if *source == list_cell => {
                    if let (Some(pred), Some((l0, l1, l2, l3))) =
                        (predicate, *self.filter_locals.borrow())
                    {
                        if item_cell.is_some() {
                            self.emit_boolean_check_loop(
                                func, *cell, *source, *pred, *item_cell,
                                item_field_cells, l0, l1, l2, l3, false,
                            );
                        }
                    }
                    self.emit_downstream_updates(func, *cell);
                }
                _ => {}
            }
        }

        // Follow PipeThrough / Derived(CellRead) chains so list updates propagate
        // through object-field wrappers (e.g., ListAppend cell=65 → PipeThrough → cell=11
        // where the ListMap sources from cell=11).
        let regular_consumers = self.downstream.get(&list_cell).unwrap_or(&empty);
        for &idx in regular_consumers {
            let node = &self.program.nodes[idx];
            match node {
                IrNode::PipeThrough { cell, source } if *source == list_cell => {
                    // The list handle flows through a PipeThrough. Copy the value
                    // and recursively propagate list downstream from the target cell.
                    func.instruction(&Instruction::GlobalGet(source.0));
                    func.instruction(&Instruction::GlobalSet(cell.0));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    self.emit_list_downstream_updates(func, *cell);
                }
                IrNode::Derived {
                    cell,
                    expr: IrExpr::CellRead(src),
                } if *src == list_cell => {
                    // List handle flows through a Derived(CellRead). Same propagation.
                    func.instruction(&Instruction::GlobalGet(src.0));
                    func.instruction(&Instruction::GlobalSet(cell.0));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    self.emit_list_downstream_updates(func, *cell);
                }
                _ => {}
            }
        }
    }

    /// After updating a cell, check for downstream nodes (e.g., MathSum) that need updating.
    /// Uses precomputed `downstream` index for O(1) consumer lookup instead of O(n) scan.
    fn emit_downstream_updates(&self, func: &mut Function, updated_cell: CellId) {
        let empty = Vec::new();
        let consumer_indices = self.downstream.get(&updated_cell).unwrap_or(&empty);
        for &idx in consumer_indices {
            let node = &self.program.nodes[idx];
            match node {
                IrNode::MathSum { cell, input } if *input == updated_cell => {
                    // Accumulate: cell += input.
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::GlobalGet(updated_cell.0));
                    func.instruction(&Instruction::F64Add);
                    func.instruction(&Instruction::GlobalSet(cell.0));
                    // Notify host.
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    // Recurse for further downstream.
                    self.emit_downstream_updates(func, *cell);
                }
                IrNode::PipeThrough { cell, source } if *source == updated_cell => {
                    func.instruction(&Instruction::GlobalGet(updated_cell.0));
                    func.instruction(&Instruction::GlobalSet(cell.0));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    // Copy text alongside f64.
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::I32Const(updated_cell.0 as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_COPY_TEXT));
                    self.emit_downstream_updates(func, *cell);
                }
                // Derived nodes that read from a cell are effectively pass-throughs.
                IrNode::Derived {
                    cell,
                    expr: IrExpr::CellRead(source),
                } if *source == updated_cell => {
                    func.instruction(&Instruction::GlobalGet(updated_cell.0));
                    func.instruction(&Instruction::GlobalSet(cell.0));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    // Copy text alongside f64.
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::I32Const(updated_cell.0 as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_COPY_TEXT));
                    self.emit_downstream_updates(func, *cell);
                }
                // Derived nodes with complex expressions referencing the updated cell.
                IrNode::Derived { cell, expr }
                    if !matches!(expr, IrExpr::CellRead(_))
                        && Self::expr_references_cell(expr, updated_cell) =>
                {
                    if let IrExpr::TextConcat(segments) = expr {
                        // TextConcat: rebuild text on the host side segment by segment.
                        // emit_text_build calls host_text_build_start/literal/cell and
                        // bumps the f64 global so signals fire.
                        self.emit_text_build(func, *cell, segments);
                    } else {
                        self.emit_expr(func, expr);
                        func.instruction(&Instruction::GlobalSet(cell.0));
                    }
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    self.emit_downstream_updates(func, *cell);
                }
                // WHEN re-evaluates when source cell changes.
                IrNode::When { cell, source, arms } if *source == updated_cell => {
                    self.emit_pattern_match(func, *source, arms, *cell, true);
                }
                // WHILE re-evaluates when source OR any dep changes.
                IrNode::While {
                    cell,
                    source,
                    deps,
                    arms,
                } if *source == updated_cell || deps.contains(&updated_cell) => {
                    self.emit_pattern_match(func, *source, arms, *cell, true);
                }
                // LATEST with non-triggered arms re-evaluates when referenced cells change.
                IrNode::Latest { target, arms } if *target != updated_cell => {
                    if let Some(arm) = arms.iter().find(|arm| {
                        arm.trigger.is_none() && Self::expr_references_cell(&arm.body, updated_cell)
                    }) {
                        let may_skip = matches!(&arm.body, IrExpr::PatternMatch { .. });
                        let text_source = Self::extract_text_source_cell(&arm.body);
                        if self.is_text_body(&arm.body) {
                            self.emit_text_setting(func, *target, &arm.body);
                            func.instruction(&Instruction::I32Const(target.0 as i32));
                            func.instruction(&Instruction::GlobalGet(target.0));
                            func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                            self.emit_downstream_updates(func, *target);
                        } else if may_skip {
                            let skip_global = self.program.cells.len() as u32;
                            self.emit_expr(func, &arm.body);
                            func.instruction(&Instruction::GlobalSet(skip_global));
                            func.instruction(&Instruction::GlobalGet(skip_global));
                            func.instruction(&Instruction::I64ReinterpretF64);
                            func.instruction(&Instruction::I64Const(SKIP_SENTINEL_BITS as i64));
                            func.instruction(&Instruction::I64Ne); // true if NOT skip sentinel
                            func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
                            func.instruction(&Instruction::GlobalGet(skip_global));
                            func.instruction(&Instruction::GlobalSet(target.0));
                            if let Some(src) = text_source {
                                func.instruction(&Instruction::I32Const(target.0 as i32));
                                func.instruction(&Instruction::I32Const(src.0 as i32));
                                func.instruction(&Instruction::Call(IMPORT_HOST_COPY_TEXT));
                            }
                            func.instruction(&Instruction::I32Const(target.0 as i32));
                            func.instruction(&Instruction::GlobalGet(target.0));
                            func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                            self.emit_downstream_updates(func, *target);
                            func.instruction(&Instruction::End);
                        } else {
                            self.emit_expr(func, &arm.body);
                            func.instruction(&Instruction::GlobalSet(target.0));
                            if let Some(src) = text_source {
                                func.instruction(&Instruction::I32Const(target.0 as i32));
                                func.instruction(&Instruction::I32Const(src.0 as i32));
                                func.instruction(&Instruction::Call(IMPORT_HOST_COPY_TEXT));
                            }
                            func.instruction(&Instruction::I32Const(target.0 as i32));
                            func.instruction(&Instruction::GlobalGet(target.0));
                            func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                            self.emit_downstream_updates(func, *target);
                        }
                    }
                }
                // ListIsEmpty re-evaluates when source changes.
                IrNode::ListIsEmpty { cell, source } if *source == updated_cell => {
                    func.instruction(&Instruction::I32Const(source.0 as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_LIST_COUNT));
                    func.instruction(&Instruction::F64Const(0.0));
                    func.instruction(&Instruction::F64Eq);
                    func.instruction(&Instruction::If(wasm_encoder::BlockType::Result(
                        ValType::F64,
                    )));
                    func.instruction(&Instruction::F64Const(1.0));
                    func.instruction(&Instruction::Else);
                    func.instruction(&Instruction::F64Const(0.0));
                    func.instruction(&Instruction::End);
                    func.instruction(&Instruction::GlobalSet(cell.0));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    self.emit_downstream_updates(func, *cell);
                }
                // TextTrim re-evaluates when source changes.
                IrNode::TextTrim { cell, source } if *source == updated_cell => {
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::I32Const(source.0 as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_TEXT_TRIM));
                    func.instruction(&Instruction::GlobalGet(source.0));
                    func.instruction(&Instruction::GlobalSet(cell.0));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    self.emit_downstream_updates(func, *cell);
                }
                // TextIsNotEmpty re-evaluates when source changes.
                IrNode::TextIsNotEmpty { cell, source } if *source == updated_cell => {
                    func.instruction(&Instruction::I32Const(source.0 as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_TEXT_IS_NOT_EMPTY));
                    func.instruction(&Instruction::GlobalSet(cell.0));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    self.emit_downstream_updates(func, *cell);
                }
                // ListAppend triggered by item cell change (downstream propagation).
                // Guard: skip when watch_cell == item, because the watch_cell handler
                // (below) already handles append with SKIP sentinel checking.
                IrNode::ListAppend {
                    cell, source, item, watch_cell, ..
                } if *item == updated_cell
                    && watch_cell.map_or(true, |w| w != *item) => {
                    // Append text from item cell to the list.
                    func.instruction(&Instruction::I32Const(source.0 as i32));
                    func.instruction(&Instruction::I32Const(item.0 as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_LIST_APPEND_TEXT));
                    self.emit_init_new_item(func, *source);
                    self.emit_list_downstream_updates(func, *cell);
                }
                // ListAppend triggered by watch_cell change (reactive dependency).
                IrNode::ListAppend {
                    cell,
                    source,
                    watch_cell: Some(watch),
                    ..
                } if *watch == updated_cell => {
                    // The watch cell changed — check for SKIP sentinel before appending.
                    // Compare bit pattern against specific SKIP sentinel (not any NaN,
                    // since text-only cells have NaN f64 values).
                    func.instruction(&Instruction::GlobalGet(watch.0));
                    func.instruction(&Instruction::I64ReinterpretF64);
                    func.instruction(&Instruction::I64Const(SKIP_SENTINEL_BITS as i64));
                    func.instruction(&Instruction::I64Eq); // true if IS skip sentinel
                    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
                    // SKIP: do nothing
                    func.instruction(&Instruction::Else);
                    // Not SKIP: append text from watch cell (the reactive source) to the list.
                    func.instruction(&Instruction::I32Const(source.0 as i32));
                    func.instruction(&Instruction::I32Const(watch.0 as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_LIST_APPEND_TEXT));
                    self.emit_init_new_item(func, *source);
                    self.emit_list_downstream_updates(func, *cell);
                    func.instruction(&Instruction::End);
                }
                // ListRetain: when predicate cell changes, re-evaluate retain.
                IrNode::ListRetain {
                    cell,
                    source,
                    predicate: Some(pred),
                    item_cell,
                    item_field_cells,
                } if *pred == updated_cell => {
                    if let (true, Some((l0, l1, l2, l3))) =
                        (item_cell.is_some(), *self.filter_locals.borrow())
                    {
                        // Per-item filtering: run filter loop.
                        self.emit_filter_loop(
                            func,
                            *cell,
                            *source,
                            *pred,
                            *item_cell,
                            item_field_cells,
                            l0,
                            l1,
                            l2,
                            l3,
                            false,
                        );
                        self.emit_list_downstream_updates(func, *cell);
                    } else if item_cell.is_none() {
                        // Binary predicate: truthy → source, falsy → empty.
                        func.instruction(&Instruction::GlobalGet(pred.0));
                        func.instruction(&Instruction::F64Const(0.0));
                        func.instruction(&Instruction::F64Ne);
                        func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
                        func.instruction(&Instruction::GlobalGet(source.0));
                        func.instruction(&Instruction::GlobalSet(cell.0));
                        func.instruction(&Instruction::Else);
                        func.instruction(&Instruction::Call(IMPORT_HOST_LIST_CREATE));
                        func.instruction(&Instruction::GlobalSet(cell.0));
                        func.instruction(&Instruction::End);
                        func.instruction(&Instruction::I32Const(cell.0 as i32));
                        func.instruction(&Instruction::GlobalGet(cell.0));
                        func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                        self.emit_list_downstream_updates(func, *cell);
                    }
                }
                // ListEvery/ListAny: when predicate cell changes, re-evaluate.
                IrNode::ListEvery {
                    cell,
                    source,
                    predicate: Some(pred),
                    item_cell,
                    item_field_cells,
                } if *pred == updated_cell => {
                    if let (true, Some((l0, l1, l2, l3))) =
                        (item_cell.is_some(), *self.filter_locals.borrow())
                    {
                        self.emit_boolean_check_loop(
                            func, *cell, *source, *pred, *item_cell,
                            item_field_cells, l0, l1, l2, l3, true,
                        );
                        self.emit_downstream_updates(func, *cell);
                    }
                }
                IrNode::ListAny {
                    cell,
                    source,
                    predicate: Some(pred),
                    item_cell,
                    item_field_cells,
                } if *pred == updated_cell => {
                    if let (true, Some((l0, l1, l2, l3))) =
                        (item_cell.is_some(), *self.filter_locals.borrow())
                    {
                        self.emit_boolean_check_loop(
                            func, *cell, *source, *pred, *item_cell,
                            item_field_cells, l0, l1, l2, l3, false,
                        );
                        self.emit_downstream_updates(func, *cell);
                    }
                }
                // ListRemove: predicate changes don't trigger re-filtering.
                // Removal only happens when the trigger event fires.
                IrNode::ListRemove { .. } => {}
                _ => {}
            }
        }
    }

    /// Check if an IrExpr references a specific cell (directly or nested).
    fn expr_references_cell(expr: &IrExpr, cell: CellId) -> bool {
        match expr {
            IrExpr::CellRead(c) => *c == cell,
            IrExpr::BinOp { lhs, rhs, .. } => {
                Self::expr_references_cell(lhs, cell) || Self::expr_references_cell(rhs, cell)
            }
            IrExpr::Compare { lhs, rhs, .. } => {
                Self::expr_references_cell(lhs, cell) || Self::expr_references_cell(rhs, cell)
            }
            IrExpr::UnaryNeg(inner) | IrExpr::Not(inner) => Self::expr_references_cell(inner, cell),
            IrExpr::FieldAccess { object, .. } => Self::expr_references_cell(object, cell),
            IrExpr::TextConcat(segs) => segs.iter().any(|s| match s {
                TextSegment::Expr(e) => Self::expr_references_cell(e, cell),
                _ => false,
            }),
            IrExpr::FunctionCall { args, .. } => {
                args.iter().any(|a| Self::expr_references_cell(a, cell))
            }
            _ => false,
        }
    }

    /// Check if an expression produces a boolean value (True/False).
    fn expr_produces_bool(expr: &IrExpr) -> bool {
        match expr {
            IrExpr::Not(_) => true,
            IrExpr::Compare { .. } => true,
            IrExpr::Constant(IrValue::Bool(_)) => true,
            _ => false,
        }
    }

    /// Extract the text source CellId from a body expression.
    /// For `CellRead(cell)`, returns the cell directly.
    /// For `PatternMatch { arms }`, returns the CellRead source from the first
    /// non-SKIP arm body. This is used by the HOLD handler to copy text from
    /// the body result cell to the HOLD cell.
    fn extract_text_source_cell(expr: &IrExpr) -> Option<CellId> {
        match expr {
            IrExpr::CellRead(cell) => Some(*cell),
            IrExpr::PatternMatch { arms, .. } => {
                for (_, arm_body) in arms {
                    if !matches!(arm_body, IrExpr::Constant(IrValue::Skip)) {
                        if let Some(cell) = Self::extract_text_source_cell(arm_body) {
                            return Some(cell);
                        }
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Emit conditional text update for a boolean cell: sets "True" or "False"
    /// based on the cell's current f64 value (0.0 = False, non-zero = True).
    fn emit_bool_text_update(&self, func: &mut Function, cell: CellId) {
        let true_idx = self.register_text_pattern("True");
        let false_idx = self.register_text_pattern("False");
        func.instruction(&Instruction::GlobalGet(cell.0));
        func.instruction(&Instruction::F64Const(0.0));
        func.instruction(&Instruction::F64Ne);
        func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
        // True case.
        func.instruction(&Instruction::I32Const(cell.0 as i32));
        func.instruction(&Instruction::I32Const(true_idx as i32));
        func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_TEXT_PATTERN));
        func.instruction(&Instruction::Else);
        // False case.
        func.instruction(&Instruction::I32Const(cell.0 as i32));
        func.instruction(&Instruction::I32Const(false_idx as i32));
        func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_TEXT_PATTERN));
        func.instruction(&Instruction::End);
    }

    /// Emit instructions that evaluate an IrExpr and leave the result on the WASM stack as f64.
    /// Uses global.get for all cell reads (no per-item memory context).
    fn emit_expr(&self, func: &mut Function, expr: &IrExpr) {
        self.emit_expr_ctx(func, expr, None);
    }

    /// Emit instructions to evaluate an IrExpr with optional per-item memory context.
    /// When `mem_ctx` is Some, template-range cells are read from linear memory.
    fn emit_expr_ctx(&self, func: &mut Function, expr: &IrExpr, mem_ctx: Option<&MemoryContext>) {
        match expr {
            IrExpr::Constant(IrValue::Number(n)) => {
                func.instruction(&Instruction::F64Const(*n));
            }
            IrExpr::Constant(IrValue::Void) => {
                func.instruction(&Instruction::F64Const(0.0));
            }
            IrExpr::Constant(IrValue::Bool(b)) => {
                func.instruction(&Instruction::F64Const(if *b { 1.0 } else { 0.0 }));
            }
            IrExpr::Constant(IrValue::Text(_)) => {
                func.instruction(&Instruction::F64Const(0.0));
            }
            IrExpr::Constant(IrValue::Tag(tag)) => {
                let encoded = self
                    .program
                    .tag_table
                    .iter()
                    .position(|t| t == tag)
                    .map(|idx| (idx + 1) as f64)
                    .unwrap_or(0.0);
                func.instruction(&Instruction::F64Const(encoded));
            }
            IrExpr::Constant(IrValue::Object(_)) => {
                func.instruction(&Instruction::F64Const(0.0));
            }
            IrExpr::Constant(IrValue::Skip) => {
                func.instruction(&Instruction::F64Const(f64::from_bits(SKIP_SENTINEL_BITS)));
            }
            IrExpr::CellRead(cell) => {
                self.emit_cell_get(func, *cell, mem_ctx);
            }
            IrExpr::BinOp { op, lhs, rhs } => {
                self.emit_expr_ctx(func, lhs, mem_ctx);
                self.emit_expr_ctx(func, rhs, mem_ctx);
                match op {
                    BinOp::Add => func.instruction(&Instruction::F64Add),
                    BinOp::Sub => func.instruction(&Instruction::F64Sub),
                    BinOp::Mul => func.instruction(&Instruction::F64Mul),
                    BinOp::Div => func.instruction(&Instruction::F64Div),
                };
            }
            IrExpr::UnaryNeg(operand) => {
                self.emit_expr_ctx(func, operand, mem_ctx);
                func.instruction(&Instruction::F64Neg);
            }
            IrExpr::Not(operand) => {
                self.emit_expr_ctx(func, operand, mem_ctx);
                func.instruction(&Instruction::F64Const(1.0));
                func.instruction(&Instruction::F64Eq);
                func.instruction(&Instruction::If(wasm_encoder::BlockType::Result(
                    ValType::F64,
                )));
                func.instruction(&Instruction::F64Const(0.0));
                func.instruction(&Instruction::Else);
                func.instruction(&Instruction::F64Const(1.0));
                func.instruction(&Instruction::End);
            }
            IrExpr::Compare { op, lhs, rhs } => {
                if matches!(op, CmpOp::Eq | CmpOp::Ne) {
                    // Text equality/inequality needs host-side text matching.
                    // Plain f64 comparison is incorrect for text cells.
                    if let (IrExpr::CellRead(cell), Some(text)) = (
                        lhs.as_ref(),
                        self.resolve_expr_text_literal_for_compare(rhs.as_ref(), 0)
                            .or_else(|| self.resolve_expr_text_statically(rhs.as_ref())),
                    ) {
                        let pattern_idx = self.register_text_pattern(&text);
                        let text_source = self.resolve_text_cell(*cell);
                        func.instruction(&Instruction::I32Const(text_source.0 as i32));
                        func.instruction(&Instruction::I32Const(pattern_idx as i32));
                        func.instruction(&Instruction::Call(IMPORT_HOST_TEXT_MATCHES));
                        if matches!(op, CmpOp::Ne) {
                            func.instruction(&Instruction::I32Eqz);
                        }
                        func.instruction(&Instruction::F64ConvertI32S);
                        return;
                    }
                    if let (IrExpr::CellRead(cell), Some(text)) = (
                        rhs.as_ref(),
                        self.resolve_expr_text_literal_for_compare(lhs.as_ref(), 0)
                            .or_else(|| self.resolve_expr_text_statically(lhs.as_ref())),
                    ) {
                        let pattern_idx = self.register_text_pattern(&text);
                        let text_source = self.resolve_text_cell(*cell);
                        func.instruction(&Instruction::I32Const(text_source.0 as i32));
                        func.instruction(&Instruction::I32Const(pattern_idx as i32));
                        func.instruction(&Instruction::Call(IMPORT_HOST_TEXT_MATCHES));
                        if matches!(op, CmpOp::Ne) {
                            func.instruction(&Instruction::I32Eqz);
                        }
                        func.instruction(&Instruction::F64ConvertI32S);
                        return;
                    }
                }
                self.emit_expr_ctx(func, lhs, mem_ctx);
                self.emit_expr_ctx(func, rhs, mem_ctx);
                match op {
                    CmpOp::Eq => func.instruction(&Instruction::F64Eq),
                    CmpOp::Ne => func.instruction(&Instruction::F64Ne),
                    CmpOp::Gt => func.instruction(&Instruction::F64Gt),
                    CmpOp::Ge => func.instruction(&Instruction::F64Ge),
                    CmpOp::Lt => func.instruction(&Instruction::F64Lt),
                    CmpOp::Le => func.instruction(&Instruction::F64Le),
                };
                func.instruction(&Instruction::F64ConvertI32S);
            }
            IrExpr::FieldAccess { object, field: _ } => {
                self.emit_expr_ctx(func, object, mem_ctx);
            }
            IrExpr::TextConcat(_) => {
                func.instruction(&Instruction::F64Const(0.0));
            }
            IrExpr::FunctionCall {
                func: _func_id,
                args: _,
            } => {
                func.instruction(&Instruction::F64Const(0.0));
            }
            IrExpr::ObjectConstruct(_) => {
                // Objects are "something" (truthy) — they exist, even if empty.
                // This is critical for filter predicates where `True => []` must
                // produce a truthy value distinguishable from SKIP (which leaves
                // the predicate at the reset value 0.0).
                func.instruction(&Instruction::F64Const(1.0));
            }
            IrExpr::ListConstruct(items) => {
                if items.is_empty() {
                    func.instruction(&Instruction::Call(IMPORT_HOST_LIST_CREATE));
                } else {
                    func.instruction(&Instruction::F64Const(0.0));
                }
            }
            IrExpr::TaggedObject { .. } => {
                func.instruction(&Instruction::F64Const(0.0));
            }
            IrExpr::PatternMatch { source, arms } => {
                // Inline pattern match: evaluate source, match patterns,
                // push result f64 on stack. SKIP arms produce the SKIP sentinel NaN.
                self.emit_pattern_match_inline(func, *source, arms, mem_ctx, 0);
            }
        }
    }

    /// Emit inline pattern match that pushes the result as an f64 on the WASM stack.
    /// Used for PatternMatch expressions in HOLD bodies.
    fn emit_pattern_match_inline(
        &self,
        func: &mut Function,
        source: CellId,
        arms: &[(IrPattern, IrExpr)],
        mem_ctx: Option<&MemoryContext>,
        idx: usize,
    ) {
        if idx >= arms.len() {
            // No arm matched: push SKIP sentinel.
            func.instruction(&Instruction::F64Const(f64::from_bits(SKIP_SENTINEL_BITS)));
            return;
        }
        let (pattern, body) = &arms[idx];
        let is_skip = matches!(body, IrExpr::Constant(IrValue::Skip));
        let has_more = idx + 1 < arms.len();

        match pattern {
            IrPattern::Tag(tag) => {
                let encoded = self
                    .program
                    .tag_table
                    .iter()
                    .position(|t| t == tag)
                    .map(|i| (i + 1) as f64)
                    .unwrap_or(0.0);
                self.emit_cell_get(func, source, mem_ctx);
                func.instruction(&Instruction::F64Const(encoded));
                func.instruction(&Instruction::F64Eq);
                func.instruction(&Instruction::If(wasm_encoder::BlockType::Result(
                    ValType::F64,
                )));
                if is_skip {
                    func.instruction(&Instruction::F64Const(f64::from_bits(SKIP_SENTINEL_BITS)));
                } else {
                    self.emit_expr_ctx(func, body, mem_ctx);
                }
                func.instruction(&Instruction::Else);
                if has_more {
                    self.emit_pattern_match_inline(func, source, arms, mem_ctx, idx + 1);
                } else {
                    func.instruction(&Instruction::F64Const(f64::from_bits(SKIP_SENTINEL_BITS)));
                }
                func.instruction(&Instruction::End);
            }
            IrPattern::Number(n) => {
                self.emit_cell_get(func, source, mem_ctx);
                func.instruction(&Instruction::F64Const(*n));
                func.instruction(&Instruction::F64Eq);
                func.instruction(&Instruction::If(wasm_encoder::BlockType::Result(
                    ValType::F64,
                )));
                if is_skip {
                    func.instruction(&Instruction::F64Const(f64::from_bits(SKIP_SENTINEL_BITS)));
                } else {
                    self.emit_expr_ctx(func, body, mem_ctx);
                }
                func.instruction(&Instruction::Else);
                if has_more {
                    self.emit_pattern_match_inline(func, source, arms, mem_ctx, idx + 1);
                } else {
                    func.instruction(&Instruction::F64Const(f64::from_bits(SKIP_SENTINEL_BITS)));
                }
                func.instruction(&Instruction::End);
            }
            IrPattern::Wildcard | IrPattern::Binding(_) => {
                if is_skip {
                    func.instruction(&Instruction::F64Const(f64::from_bits(SKIP_SENTINEL_BITS)));
                } else {
                    self.emit_expr_ctx(func, body, mem_ctx);
                }
            }
            IrPattern::Text(text) => {
                let pattern_idx = self.register_text_pattern(text);
                let text_source = self.resolve_text_cell(source);
                func.instruction(&Instruction::I32Const(text_source.0 as i32));
                func.instruction(&Instruction::I32Const(pattern_idx as i32));
                func.instruction(&Instruction::Call(IMPORT_HOST_TEXT_MATCHES));
                func.instruction(&Instruction::If(wasm_encoder::BlockType::Result(
                    ValType::F64,
                )));
                if is_skip {
                    func.instruction(&Instruction::F64Const(f64::from_bits(SKIP_SENTINEL_BITS)));
                } else {
                    self.emit_expr_ctx(func, body, mem_ctx);
                }
                func.instruction(&Instruction::Else);
                if has_more {
                    self.emit_pattern_match_inline(func, source, arms, mem_ctx, idx + 1);
                } else {
                    func.instruction(&Instruction::F64Const(f64::from_bits(SKIP_SENTINEL_BITS)));
                }
                func.instruction(&Instruction::End);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Per-item cell helpers
    // -----------------------------------------------------------------------

    /// Emit instructions to read a cell value (f64) onto the stack.
    /// Always reads from WASM global (used as workspace for per-item cells).
    fn emit_cell_get(&self, func: &mut Function, cell: CellId, _mem_ctx: Option<&MemoryContext>) {
        func.instruction(&Instruction::GlobalGet(cell.0));
    }

    /// Emit instructions to write a cell value. Value must already be on the stack.
    /// Always writes to WASM global (used as workspace for per-item cells).
    fn emit_cell_set(&self, func: &mut Function, cell: CellId, _mem_ctx: Option<&MemoryContext>) {
        func.instruction(&Instruction::GlobalSet(cell.0));
    }

    /// Collect info for ALL ListMap nodes in the program.
    fn find_all_list_map_infos(&self) -> Vec<ListMapInfo> {
        let mut infos = Vec::new();
        for node in &self.program.nodes {
            if let IrNode::ListMap {
                cell,
                source,
                item_cell,
                template_cell_range,
                template_event_range,
                ..
            } = node
            {
                infos.push(ListMapInfo {
                    cell: *cell,
                    source: *source,
                    item_cell: *item_cell,
                    cell_range: *template_cell_range,
                    event_range: *template_event_range,
                });
            }
        }
        infos
    }

    /// Find the first ListMap node and return its template ranges (backward compat).
    /// Build a MemoryContext for a specific ListMap.
    fn build_template_context(info: &ListMapInfo, item_idx_local: u32) -> Option<MemoryContext> {
        let cell_count = info.cell_range.1 - info.cell_range.0;
        if cell_count == 0 {
            return None;
        }
        Some(MemoryContext {
            item_idx_local,
            cell_start: info.cell_range.0,
            cell_end: info.cell_range.1,
        })
    }

    /// Find which ListMap is downstream of a given list source cell.
    /// Traces through ListRetain/PipeThrough chains to find the ListMap
    /// that ultimately reads from this source.
    fn find_list_map_for_source(&self, source: CellId) -> Option<CellId> {
        let all_maps = self.find_all_list_map_infos();
        // Direct match: ListMap.source == source
        for info in &all_maps {
            if info.source == source {
                return Some(info.cell);
            }
        }
        // Trace through ListRetain/PipeThrough/ListMap chains.
        // Build a mapping: cell → downstream cells.
        for info in &all_maps {
            let mut current = info.source;
            for _ in 0..100 {
                // safety limit
                if current == source {
                    return Some(info.cell);
                }
                // Find what produces `current`.
                let mut found = false;
                for node in &self.program.nodes {
                    match node {
                        IrNode::ListRetain {
                            cell: output,
                            source: src,
                            ..
                        }
                        | IrNode::PipeThrough {
                            cell: output,
                            source: src,
                        } if *output == current => {
                            current = *src;
                            found = true;
                            break;
                        }
                        IrNode::ListMap {
                            cell: output,
                            source: src,
                            ..
                        } if *output == current => {
                            current = *src;
                            found = true;
                            break;
                        }
                        _ => {}
                    }
                }
                if !found {
                    break;
                }
            }
        }
        None
    }

    /// After appending an item, call init_item to initialize the new item's
    /// cells before downstream updates (retain filters) run.
    /// Passes the correct map_cell so init_item initializes the right ListMap.
    fn emit_init_new_item(&self, func: &mut Function, source: CellId) {
        if let Some(map_cell) = self.find_list_map_for_source(source) {
            // Get new item's index: count - 1 (item was just appended).
            func.instruction(&Instruction::I32Const(source.0 as i32));
            func.instruction(&Instruction::Call(IMPORT_HOST_LIST_COUNT));
            func.instruction(&Instruction::F64Const(1.0));
            func.instruction(&Instruction::F64Sub);
            func.instruction(&Instruction::I32TruncF64S);
            // Push map_cell identifier.
            func.instruction(&Instruction::I32Const(map_cell.0 as i32));
            func.instruction(&Instruction::Call(FN_INIT_ITEM));
        }
    }

    // -----------------------------------------------------------------------
    // Per-item retain (filter loop)
    // -----------------------------------------------------------------------

    /// Check if any ListRetain, ListEvery, ListAny, or ListRemove has per-item filtering.
    fn has_per_item_filter(&self) -> bool {
        self.program.nodes.iter().any(|node| {
            matches!(node, IrNode::ListRetain { item_cell: Some(_), .. })
            || matches!(node, IrNode::ListEvery { item_cell: Some(_), .. })
            || matches!(node, IrNode::ListAny { item_cell: Some(_), .. })
            || matches!(node, IrNode::ListRemove { item_field_cells, .. } if !item_field_cells.is_empty())
        })
    }

    /// Find the ListMap's item_cell for looking up template field cells.
    /// If `for_source` is provided, finds ALL ListMaps whose source chain
    /// includes that cell, then picks the one with the largest template range.
    /// The largest template is the rendering template — it contains the interactive
    /// elements (checkboxes, buttons) whose events update per-item field cells.
    /// When multiple ListMaps exist on the same source (e.g., one for data
    /// processing, one for rendering), the filter must read from the rendering
    /// template to see event-driven updates.
    fn find_list_map_item_cell_for(&self, for_source: Option<CellId>) -> Option<CellId> {
        let all_maps = self.find_all_list_map_infos();

        if let Some(source) = for_source {
            // Find ALL ListMaps connected to source (directly or via chains).
            let mut connected: Vec<&ListMapInfo> = Vec::new();

            for info in &all_maps {
                // Direct match.
                if info.source == source {
                    connected.push(info);
                    continue;
                }
                // Chain match: trace backwards from ListMap.source through
                // ListRetain/PipeThrough/ListMap to find the original source.
                let mut current = info.source;
                for _ in 0..100 {
                    if current == source {
                        connected.push(info);
                        break;
                    }
                    let mut found = false;
                    for node in &self.program.nodes {
                        match node {
                            IrNode::ListRetain {
                                cell: output,
                                source: src,
                                ..
                            }
                            | IrNode::PipeThrough {
                                cell: output,
                                source: src,
                            }
                            | IrNode::ListMap {
                                cell: output,
                                source: src,
                                ..
                            } if *output == current => {
                                current = *src;
                                found = true;
                                break;
                            }
                            _ => {}
                        }
                    }
                    if !found {
                        break;
                    }
                }
            }

            // Prefer the ListMap with the largest template range (rendering template).
            if let Some(best) = connected
                .iter()
                .max_by_key(|info| info.cell_range.1 - info.cell_range.0)
            {
                return Some(best.item_cell);
            }
        }

        // Fallback to largest ListMap (consistent with above preference).
        all_maps
            .iter()
            .max_by_key(|info| info.cell_range.1 - info.cell_range.0)
            .map(|info| info.item_cell)
    }

    /// Emit a WASM filter loop that iterates source list items, evaluates the
    /// predicate per-item, and builds a filtered list via host_list_copy_item.
    ///
    /// When `invert` is false (retain), items where predicate is truthy are kept.
    /// When `invert` is true (remove), items where predicate is truthy are removed
    /// (i.e., items where predicate is 0.0 are kept). The predicate is reset to 0.0
    /// before each iteration so SKIP (which doesn't update the cell) defaults to "keep".
    ///
    /// `local_new_list`: f64 local for new list ID
    /// `local_count`: i32 local for item count
    /// `local_i`: i32 local for loop counter
    fn emit_filter_loop(
        &self,
        func: &mut Function,
        output_cell: CellId,
        source_cell: CellId,
        predicate: CellId,
        item_cell: Option<CellId>,
        item_field_cells: &[(String, CellId)],
        local_new_list: u32,
        local_count: u32,
        local_i: u32,
        local_mem_idx: u32,
        invert: bool,
    ) {
        let list_map_item_cell = match self.find_list_map_item_cell_for(Some(source_cell)) {
            Some(c) => c,
            None => return,
        };

        // 1. Create new empty list
        func.instruction(&Instruction::Call(IMPORT_HOST_LIST_CREATE));
        func.instruction(&Instruction::LocalSet(local_new_list));

        // 2. Get item count from source list
        func.instruction(&Instruction::I32Const(source_cell.0 as i32));
        func.instruction(&Instruction::Call(IMPORT_HOST_LIST_COUNT));
        func.instruction(&Instruction::I32TruncF64S);
        func.instruction(&Instruction::LocalSet(local_count));

        // 3. Loop: i = 0
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::LocalSet(local_i));

        // block $break { loop $continue { ... } }
        func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty)); // $break
        func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty)); // $continue

        // if (i >= count) break
        func.instruction(&Instruction::LocalGet(local_i));
        func.instruction(&Instruction::LocalGet(local_count));
        func.instruction(&Instruction::I32GeS);
        func.instruction(&Instruction::BrIf(1)); // br $break

        // For inverted filter (remove): reset predicate to 0.0 before evaluating.
        // SKIP doesn't update the cell, so without reset the previous iteration's
        // truthy value would bleed into the next iteration.
        if invert {
            func.instruction(&Instruction::F64Const(0.0));
            func.instruction(&Instruction::GlobalSet(predicate.0));
        }

        // Read per-item field values from host-side ItemCellStore.
        // Use host_list_item_memory_index to get the correct item slot index for each
        // list position. After List/remove, the source list becomes index-based — position 0
        // may map to original item index 1 if item 0 was removed.
        func.instruction(&Instruction::I32Const(source_cell.0 as i32));
        func.instruction(&Instruction::LocalGet(local_i));
        func.instruction(&Instruction::Call(IMPORT_HOST_LIST_ITEM_MEMORY_INDEX));
        func.instruction(&Instruction::LocalSet(local_mem_idx));

        // Set item context so host_get_cell_f64 reads from per-item store.
        func.instruction(&Instruction::LocalGet(local_mem_idx));
        func.instruction(&Instruction::Call(IMPORT_HOST_SET_ITEM_CONTEXT));

        for (field_name, sub_cell) in item_field_cells {
            // Find the template field cell for this field name.
            let template_field_cell = self
                .program
                .cell_field_cells
                .get(&list_map_item_cell)
                .and_then(|fields| fields.get(field_name))
                .copied();
            if let Some(tfc) = template_field_cell {
                // Read per-item cell value via host import.
                func.instruction(&Instruction::I32Const(tfc.0 as i32));
                func.instruction(&Instruction::Call(IMPORT_HOST_GET_CELL_F64));
                func.instruction(&Instruction::GlobalSet(sub_cell.0));
            }
        }

        // Clear item context after reading field values.
        func.instruction(&Instruction::Call(IMPORT_HOST_CLEAR_ITEM_CONTEXT));

        // For numeric list items (no field cells), load the item value from the host.
        // The item cell's global needs the actual item value for predicates like `n == 2`.
        if item_field_cells.is_empty() {
            if let Some(ic) = item_cell {
                // host_list_get_item_f64(source_cell, i) -> f64
                func.instruction(&Instruction::I32Const(source_cell.0 as i32));
                func.instruction(&Instruction::LocalGet(local_i));
                func.instruction(&Instruction::Call(IMPORT_HOST_LIST_GET_ITEM_F64));
                func.instruction(&Instruction::GlobalSet(ic.0));
            }
        }

        // Re-evaluate any intermediate Derived cells that reference field sub-cells.
        // (e.g., Bool/not(item.completed) → Not(CellRead(completed_sub_cell)))
        let mut field_cell_ids: Vec<CellId> = item_field_cells.iter().map(|(_, c)| *c).collect();
        // Also include item_cell when it has no field cells (numeric items).
        if item_field_cells.is_empty() {
            if let Some(ic) = item_cell {
                field_cell_ids.push(ic);
            }
        }
        for node in &self.program.nodes {
            if let IrNode::Derived { cell, expr } = node {
                if field_cell_ids
                    .iter()
                    .any(|fc| Self::expr_references_cell(expr, *fc))
                {
                    self.emit_expr(func, expr);
                    func.instruction(&Instruction::GlobalSet(cell.0));
                }
            }
        }

        // Evaluate the predicate.
        // Find if the predicate is defined by a WHILE or WHEN node.
        let pred_node = self.find_node_for_cell(predicate);
        match pred_node {
            Some(IrNode::While { source, arms, .. }) => {
                self.emit_pattern_arms_no_notify(func, *source, arms, predicate, 0, invert);
            }
            Some(IrNode::When { source, arms, .. }) => {
                self.emit_pattern_arms_no_notify(func, *source, arms, predicate, 0, invert);
            }
            _ => {
                // Predicate is directly a field sub-cell or simple derived —
                // already set by the field read or derived re-evaluation above.
            }
        }

        // Retain: copy items where predicate is truthy (non-zero).
        // Remove (invert): copy items where predicate is falsy (zero).
        func.instruction(&Instruction::GlobalGet(predicate.0));
        func.instruction(&Instruction::F64Const(0.0));
        if invert {
            func.instruction(&Instruction::F64Eq);
        } else {
            func.instruction(&Instruction::F64Ne);
        }
        func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
        // host_list_copy_item(new_list_id, source_cell_id, item_idx)
        func.instruction(&Instruction::LocalGet(local_new_list));
        func.instruction(&Instruction::I32Const(source_cell.0 as i32));
        func.instruction(&Instruction::LocalGet(local_i));
        func.instruction(&Instruction::Call(IMPORT_HOST_LIST_COPY_ITEM));
        func.instruction(&Instruction::End); // end if

        // i++
        func.instruction(&Instruction::LocalGet(local_i));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(local_i));
        func.instruction(&Instruction::Br(0)); // br $continue

        func.instruction(&Instruction::End); // end loop
        func.instruction(&Instruction::End); // end block

        // 4. Set output cell to new list
        func.instruction(&Instruction::LocalGet(local_new_list));
        func.instruction(&Instruction::GlobalSet(output_cell.0));
        func.instruction(&Instruction::I32Const(output_cell.0 as i32));
        func.instruction(&Instruction::GlobalGet(output_cell.0));
        func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
    }

    /// Emit a WASM boolean check loop that iterates source list items, evaluates the
    /// predicate per-item, and returns 1.0 or 0.0.
    ///
    /// When `is_every` is true (List/every): starts at 1.0, sets to 0.0 and breaks on first falsy.
    /// When `is_every` is false (List/any): starts at 0.0, sets to 1.0 and breaks on first truthy.
    ///
    /// Shares the per-item field loading and predicate evaluation logic with emit_filter_loop.
    fn emit_boolean_check_loop(
        &self,
        func: &mut Function,
        output_cell: CellId,
        source_cell: CellId,
        predicate: CellId,
        item_cell: Option<CellId>,
        item_field_cells: &[(String, CellId)],
        local_result: u32,
        local_count: u32,
        local_i: u32,
        local_mem_idx: u32,
        is_every: bool,
    ) {
        let list_map_item_cell = match self.find_list_map_item_cell_for(Some(source_cell)) {
            Some(c) => c,
            None => {
                // No ListMap: result depends on whether the list is empty.
                // every([]) = true, any([]) = false.
                let initial = if is_every { 1.0 } else { 0.0 };
                func.instruction(&Instruction::F64Const(initial));
                func.instruction(&Instruction::GlobalSet(output_cell.0));
                func.instruction(&Instruction::I32Const(output_cell.0 as i32));
                func.instruction(&Instruction::GlobalGet(output_cell.0));
                func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                return;
            }
        };

        // 1. Initialize result: every starts true, any starts false.
        let initial = if is_every { 1.0 } else { 0.0 };
        func.instruction(&Instruction::F64Const(initial));
        func.instruction(&Instruction::LocalSet(local_result));

        // 2. Get item count from source list.
        func.instruction(&Instruction::I32Const(source_cell.0 as i32));
        func.instruction(&Instruction::Call(IMPORT_HOST_LIST_COUNT));
        func.instruction(&Instruction::I32TruncF64S);
        func.instruction(&Instruction::LocalSet(local_count));

        // 3. Loop: i = 0
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::LocalSet(local_i));

        // block $break { loop $continue { ... } }
        func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));

        // if (i >= count) break
        func.instruction(&Instruction::LocalGet(local_i));
        func.instruction(&Instruction::LocalGet(local_count));
        func.instruction(&Instruction::I32GeS);
        func.instruction(&Instruction::BrIf(1));

        // Read per-item field values from host-side ItemCellStore.
        func.instruction(&Instruction::I32Const(source_cell.0 as i32));
        func.instruction(&Instruction::LocalGet(local_i));
        func.instruction(&Instruction::Call(IMPORT_HOST_LIST_ITEM_MEMORY_INDEX));
        func.instruction(&Instruction::LocalSet(local_mem_idx));

        // Set item context so host_get_cell_f64 reads from per-item store.
        func.instruction(&Instruction::LocalGet(local_mem_idx));
        func.instruction(&Instruction::Call(IMPORT_HOST_SET_ITEM_CONTEXT));

        for (field_name, sub_cell) in item_field_cells {
            let template_field_cell = self
                .program
                .cell_field_cells
                .get(&list_map_item_cell)
                .and_then(|fields| fields.get(field_name))
                .copied();
            if let Some(tfc) = template_field_cell {
                // Read per-item cell value via host import.
                func.instruction(&Instruction::I32Const(tfc.0 as i32));
                func.instruction(&Instruction::Call(IMPORT_HOST_GET_CELL_F64));
                func.instruction(&Instruction::GlobalSet(sub_cell.0));
            }
        }

        // Clear item context after reading field values.
        func.instruction(&Instruction::Call(IMPORT_HOST_CLEAR_ITEM_CONTEXT));

        // For numeric list items (no field cells), load item value.
        if item_field_cells.is_empty() {
            if let Some(ic) = item_cell {
                func.instruction(&Instruction::I32Const(source_cell.0 as i32));
                func.instruction(&Instruction::LocalGet(local_i));
                func.instruction(&Instruction::Call(IMPORT_HOST_LIST_GET_ITEM_F64));
                func.instruction(&Instruction::GlobalSet(ic.0));
            }
        }

        // Re-evaluate intermediate Derived cells.
        let mut field_cell_ids: Vec<CellId> = item_field_cells.iter().map(|(_, c)| *c).collect();
        if item_field_cells.is_empty() {
            if let Some(ic) = item_cell {
                field_cell_ids.push(ic);
            }
        }
        for node in &self.program.nodes {
            if let IrNode::Derived { cell, expr } = node {
                if field_cell_ids
                    .iter()
                    .any(|fc| Self::expr_references_cell(expr, *fc))
                {
                    self.emit_expr(func, expr);
                    func.instruction(&Instruction::GlobalSet(cell.0));
                }
            }
        }

        // Evaluate the predicate.
        let pred_node = self.find_node_for_cell(predicate);
        match pred_node {
            Some(IrNode::While { source, arms, .. }) => {
                self.emit_pattern_arms_no_notify(func, *source, arms, predicate, 0, false);
            }
            Some(IrNode::When { source, arms, .. }) => {
                self.emit_pattern_arms_no_notify(func, *source, arms, predicate, 0, false);
            }
            _ => {}
        }

        // Check predicate and short-circuit.
        func.instruction(&Instruction::GlobalGet(predicate.0));
        func.instruction(&Instruction::F64Const(0.0));
        if is_every {
            // every: if predicate is falsy → result = 0.0, break
            func.instruction(&Instruction::F64Eq);
            func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
            func.instruction(&Instruction::F64Const(0.0));
            func.instruction(&Instruction::LocalSet(local_result));
            func.instruction(&Instruction::Br(2)); // break out of block
            func.instruction(&Instruction::End);
        } else {
            // any: if predicate is truthy → result = 1.0, break
            func.instruction(&Instruction::F64Ne);
            func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
            func.instruction(&Instruction::F64Const(1.0));
            func.instruction(&Instruction::LocalSet(local_result));
            func.instruction(&Instruction::Br(2)); // break out of block
            func.instruction(&Instruction::End);
        }

        // i++
        func.instruction(&Instruction::LocalGet(local_i));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(local_i));
        func.instruction(&Instruction::Br(0)); // br $continue

        func.instruction(&Instruction::End); // end loop
        func.instruction(&Instruction::End); // end block

        // 4. Set output cell to result.
        func.instruction(&Instruction::LocalGet(local_result));
        func.instruction(&Instruction::GlobalSet(output_cell.0));
        func.instruction(&Instruction::I32Const(output_cell.0 as i32));
        func.instruction(&Instruction::GlobalGet(output_cell.0));
        func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
    }

    /// Like `emit_pattern_arms` but without host notification or downstream propagation.
    /// Used inside filter loops where we only need the predicate value.
    ///
    /// When `force_truthy` is false (retain mode): stores the body's actual f64 value as the
    /// predicate, so the caller can distinguish truthy from falsy results.
    ///
    /// When `force_truthy` is true (remove mode): discards the body value and stores 1.0
    /// when a non-SKIP arm matches. This is necessary because some body expressions
    /// (e.g., `[]` = empty object) produce 0.0 which would be indistinguishable from "no match".
    fn emit_pattern_arms_no_notify(
        &self,
        func: &mut Function,
        source: CellId,
        arms: &[(IrPattern, IrExpr)],
        target: CellId,
        idx: usize,
        force_truthy: bool,
    ) {
        if idx >= arms.len() {
            return;
        }

        let (pattern, body) = &arms[idx];
        let is_skip = matches!(body, IrExpr::Constant(IrValue::Skip));
        let has_more = idx + 1 < arms.len();

        match pattern {
            IrPattern::Tag(tag) => {
                let encoded = self
                    .program
                    .tag_table
                    .iter()
                    .position(|t| t == tag)
                    .map(|i| (i + 1) as f64)
                    .unwrap_or(0.0);
                func.instruction(&Instruction::GlobalGet(source.0));
                func.instruction(&Instruction::F64Const(encoded));
                func.instruction(&Instruction::F64Eq);
                func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
                if !is_skip {
                    self.emit_expr(func, body);
                    if force_truthy {
                        func.instruction(&Instruction::Drop);
                        func.instruction(&Instruction::F64Const(1.0));
                    }
                    func.instruction(&Instruction::GlobalSet(target.0));
                }
                if has_more {
                    func.instruction(&Instruction::Else);
                    self.emit_pattern_arms_no_notify(
                        func,
                        source,
                        arms,
                        target,
                        idx + 1,
                        force_truthy,
                    );
                }
                func.instruction(&Instruction::End);
            }
            IrPattern::Number(n) => {
                func.instruction(&Instruction::GlobalGet(source.0));
                func.instruction(&Instruction::F64Const(*n));
                func.instruction(&Instruction::F64Eq);
                func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
                if !is_skip {
                    self.emit_expr(func, body);
                    if force_truthy {
                        func.instruction(&Instruction::Drop);
                        func.instruction(&Instruction::F64Const(1.0));
                    }
                    func.instruction(&Instruction::GlobalSet(target.0));
                }
                if has_more {
                    func.instruction(&Instruction::Else);
                    self.emit_pattern_arms_no_notify(
                        func,
                        source,
                        arms,
                        target,
                        idx + 1,
                        force_truthy,
                    );
                }
                func.instruction(&Instruction::End);
            }
            IrPattern::Text(text) => {
                let pattern_idx = self.register_text_pattern(text);
                let text_source = self.resolve_text_cell(source);
                func.instruction(&Instruction::I32Const(text_source.0 as i32));
                func.instruction(&Instruction::I32Const(pattern_idx as i32));
                func.instruction(&Instruction::Call(IMPORT_HOST_TEXT_MATCHES));
                func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
                if !is_skip {
                    self.emit_expr(func, body);
                    if force_truthy {
                        func.instruction(&Instruction::Drop);
                        func.instruction(&Instruction::F64Const(1.0));
                    }
                    func.instruction(&Instruction::GlobalSet(target.0));
                }
                if has_more {
                    func.instruction(&Instruction::Else);
                    self.emit_pattern_arms_no_notify(
                        func,
                        source,
                        arms,
                        target,
                        idx + 1,
                        force_truthy,
                    );
                }
                func.instruction(&Instruction::End);
            }
            IrPattern::Wildcard | IrPattern::Binding(_) => {
                if !is_skip {
                    self.emit_expr(func, body);
                    if force_truthy {
                        func.instruction(&Instruction::Drop);
                        func.instruction(&Instruction::F64Const(1.0));
                    }
                    func.instruction(&Instruction::GlobalSet(target.0));
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Per-item WASM function emitters
    // -----------------------------------------------------------------------

    /// Emit `init_item(item_idx: i32, map_cell: i32)`.
    /// Initializes template cells for a new item in the specified ListMap.
    /// Dispatches by map_cell to handle multiple ListMaps.
    fn emit_init_item(&self) -> Function {
        // local 0 = item_idx, local 1 = map_cell
        let mut func = Function::new([]);
        let all_maps = self.find_all_list_map_infos();

        if all_maps.is_empty() {
            func.instruction(&Instruction::End);
            return func;
        }

        // Set item context so host routes updates to per-item Mutables.
        func.instruction(&Instruction::LocalGet(0)); // item_idx
        func.instruction(&Instruction::Call(IMPORT_HOST_SET_ITEM_CONTEXT));

        // Dispatch by map_cell: each ListMap gets its own init block.
        for info in &all_maps {
            let mem_ctx = match Self::build_template_context(info, 0) {
                Some(ctx) => ctx,
                None => continue,
            };

            // if (local.1 == map_cell_id) { ... init cells ... }
            func.instruction(&Instruction::LocalGet(1)); // map_cell
            func.instruction(&Instruction::I32Const(info.cell.0 as i32));
            func.instruction(&Instruction::I32Eq);
            func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));

            // Initialize each template-scoped node for this ListMap.
            self.emit_init_item_body(&mut func, &mem_ctx);

            func.instruction(&Instruction::End); // end if
        }

        // Clear item context.
        func.instruction(&Instruction::Call(IMPORT_HOST_CLEAR_ITEM_CONTEXT));
        func.instruction(&Instruction::End);
        func
    }

    /// Emit the body of init_item for a single ListMap's template range.
    fn emit_init_item_body(&self, func: &mut Function, mem_ctx: &MemoryContext) {
        for node in &self.program.nodes {
            match node {
                IrNode::Hold { cell, init, .. }
                    if cell.0 >= mem_ctx.cell_start && cell.0 < mem_ctx.cell_end =>
                {
                    // Evaluate init expr → store to global workspace.
                    self.emit_expr_ctx(func, init, Some(mem_ctx));
                    self.emit_cell_set(func, *cell, Some(mem_ctx));
                    // Notify host of per-item cell value.
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    self.emit_cell_get(func, *cell, Some(mem_ctx));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    // Copy text from init source cell to HOLD cell so per-item
                    // text (e.g., todo title) propagates through HOLD.
                    if let IrExpr::CellRead(src) = init {
                        func.instruction(&Instruction::I32Const(cell.0 as i32));
                        func.instruction(&Instruction::I32Const(src.0 as i32));
                        func.instruction(&Instruction::Call(IMPORT_HOST_COPY_TEXT));
                    } else if let Some(text) = self.resolve_expr_text_statically(init) {
                        if !text.is_empty() {
                            let pattern_idx = self.register_text_pattern(&text);
                            func.instruction(&Instruction::I32Const(cell.0 as i32));
                            func.instruction(&Instruction::I32Const(pattern_idx as i32));
                            func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_TEXT_PATTERN));
                        }
                    }
                }
                IrNode::Derived { cell, expr }
                    if cell.0 >= mem_ctx.cell_start && cell.0 < mem_ctx.cell_end =>
                {
                    self.emit_expr_ctx(func, expr, Some(mem_ctx));
                    self.emit_cell_set(func, *cell, Some(mem_ctx));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    self.emit_cell_get(func, *cell, Some(mem_ctx));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    // Set text: CellRead copies from source, TextConcat builds
                    // dynamically, static expressions use pattern.
                    if let IrExpr::CellRead(src) = expr {
                        func.instruction(&Instruction::I32Const(cell.0 as i32));
                        func.instruction(&Instruction::I32Const(src.0 as i32));
                        func.instruction(&Instruction::Call(IMPORT_HOST_COPY_TEXT));
                    } else if let IrExpr::TextConcat(segments) = expr {
                        if segments
                            .iter()
                            .any(|s| matches!(s, TextSegment::Expr(IrExpr::CellRead(_))))
                        {
                            self.emit_text_build_ctx(func, *cell, segments, Some(mem_ctx));
                        } else if let Some(text) = self.resolve_expr_text_statically(expr) {
                            if !text.is_empty() {
                                let pattern_idx = self.register_text_pattern(&text);
                                func.instruction(&Instruction::I32Const(cell.0 as i32));
                                func.instruction(&Instruction::I32Const(pattern_idx as i32));
                                func.instruction(&Instruction::Call(
                                    IMPORT_HOST_SET_CELL_TEXT_PATTERN,
                                ));
                            }
                        }
                    } else if let Some(text) = self.resolve_expr_text_statically(expr) {
                        if !text.is_empty() {
                            let pattern_idx = self.register_text_pattern(&text);
                            func.instruction(&Instruction::I32Const(cell.0 as i32));
                            func.instruction(&Instruction::I32Const(pattern_idx as i32));
                            func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_TEXT_PATTERN));
                        }
                    }
                }
                IrNode::When { cell, source, arms }
                    if cell.0 >= mem_ctx.cell_start && cell.0 < mem_ctx.cell_end =>
                {
                    self.emit_pattern_match_ctx(func, *source, arms, *cell, Some(mem_ctx), true);
                }
                IrNode::While {
                    cell, source, arms, ..
                } if cell.0 >= mem_ctx.cell_start && cell.0 < mem_ctx.cell_end => {
                    self.emit_pattern_match_ctx(func, *source, arms, *cell, Some(mem_ctx), true);
                }
                IrNode::Latest { target, arms }
                    if target.0 >= mem_ctx.cell_start && target.0 < mem_ctx.cell_end =>
                {
                    // Initialize with the first arm's body (static or triggered).
                    if let Some(arm) = arms.first() {
                        if self.is_text_body(&arm.body) {
                            self.emit_text_setting_ctx(
                                func,
                                *target,
                                &arm.body,
                                Some(mem_ctx),
                            );
                        } else {
                            self.emit_expr_ctx(func, &arm.body, Some(mem_ctx));
                            self.emit_cell_set(func, *target, Some(mem_ctx));
                        }
                        func.instruction(&Instruction::I32Const(target.0 as i32));
                        self.emit_cell_get(func, *target, Some(mem_ctx));
                        func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                        // Copy text from the body's source cell.
                        if let IrExpr::CellRead(src) = &arm.body {
                            func.instruction(&Instruction::I32Const(target.0 as i32));
                            func.instruction(&Instruction::I32Const(src.0 as i32));
                            func.instruction(&Instruction::Call(IMPORT_HOST_COPY_TEXT));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Emit `on_item_event(item_idx: i32, event_id: i32)`.
    /// Handles per-item events using br_table dispatch.
    /// Supports multiple ListMaps by using the correct MemoryContext per event.
    fn emit_on_item_event(&self) -> Function {
        // Check if any ListRemove uses per-item events (Case 1: predicate is None,
        // trigger is template-scoped).
        let has_per_item_remove = self.program.nodes.iter().any(|node| {
            matches!(
                node,
                IrNode::ListRemove {
                    predicate: None,
                    ..
                }
            )
        });

        // local 0 = item_idx, local 1 = event_id
        // If per-item remove exists, add locals for the remove loop + downstream filter:
        // local 2 = new_list_id (f64), local 3 = count (i32), local 4 = i (i32), local 5 = mem_idx (i32)
        let locals: Vec<(u32, ValType)> = if has_per_item_remove {
            vec![(1, ValType::F64), (3, ValType::I32)]
        } else {
            vec![]
        };
        let mut func = Function::new(locals);
        if has_per_item_remove {
            *self.filter_locals.borrow_mut() = Some((2, 3, 4, 5));
        }

        let all_maps = self.find_all_list_map_infos();
        if all_maps.is_empty() {
            if has_per_item_remove {
                *self.filter_locals.borrow_mut() = None;
            }
            func.instruction(&Instruction::End);
            return func;
        }

        // Build a map: event_id → MemoryContext (which ListMap owns this event).
        // An event belongs to the ListMap whose cell range contains the triggered node's cell,
        // or whose event range contains the event.
        let mut event_to_mem_ctx: HashMap<u32, MemoryContext> = HashMap::new();

        for info in &all_maps {
            let mem_ctx = match Self::build_template_context(info, 0) {
                Some(ctx) => ctx,
                None => continue,
            };

            // Collect events that trigger template-scoped nodes in this ListMap.
            for node in &self.program.nodes {
                match node {
                    IrNode::Then { trigger, cell, .. }
                        if cell.0 >= mem_ctx.cell_start && cell.0 < mem_ctx.cell_end =>
                    {
                        event_to_mem_ctx.entry(trigger.0).or_insert(MemoryContext {
                            item_idx_local: mem_ctx.item_idx_local,
                            cell_start: mem_ctx.cell_start,
                            cell_end: mem_ctx.cell_end,
                        });
                    }
                    IrNode::Hold {
                        trigger_bodies,
                        cell,
                        ..
                    } if cell.0 >= mem_ctx.cell_start && cell.0 < mem_ctx.cell_end => {
                        for (t, _) in trigger_bodies {
                            event_to_mem_ctx.entry(t.0).or_insert(MemoryContext {
                                item_idx_local: mem_ctx.item_idx_local,
                                cell_start: mem_ctx.cell_start,
                                cell_end: mem_ctx.cell_end,
                            });
                        }
                    }
                    IrNode::Latest { target, arms }
                        if target.0 >= mem_ctx.cell_start && target.0 < mem_ctx.cell_end =>
                    {
                        for arm in arms {
                            if let Some(t) = arm.trigger {
                                event_to_mem_ctx.entry(t.0).or_insert(MemoryContext {
                                    item_idx_local: mem_ctx.item_idx_local,
                                    cell_start: mem_ctx.cell_start,
                                    cell_end: mem_ctx.cell_end,
                                });
                            }
                        }
                    }
                    // Per-item ListRemove: trigger is template-scoped, cell is global.
                    IrNode::ListRemove {
                        trigger,
                        predicate: None,
                        ..
                    } if trigger.0 >= info.event_range.0 && trigger.0 < info.event_range.1 => {
                        event_to_mem_ctx.entry(trigger.0).or_insert(MemoryContext {
                            item_idx_local: mem_ctx.item_idx_local,
                            cell_start: mem_ctx.cell_start,
                            cell_end: mem_ctx.cell_end,
                        });
                    }
                    _ => {}
                }
            }

            // Also include events that carry template payload cells.
            for (event_idx, event_info) in self.program.events.iter().enumerate() {
                let has_template_payload = event_info
                    .payload_cells
                    .iter()
                    .any(|cell| cell.0 >= mem_ctx.cell_start && cell.0 < mem_ctx.cell_end);
                if has_template_payload {
                    event_to_mem_ctx.entry(event_idx as u32).or_insert(MemoryContext {
                        item_idx_local: mem_ctx.item_idx_local,
                        cell_start: mem_ctx.cell_start,
                        cell_end: mem_ctx.cell_end,
                    });
                }
            }
        }

        if event_to_mem_ctx.is_empty() {
            if has_per_item_remove {
                *self.filter_locals.borrow_mut() = None;
            }
            func.instruction(&Instruction::End);
            return func;
        }

        // Set item context.
        func.instruction(&Instruction::LocalGet(0)); // item_idx
        func.instruction(&Instruction::Call(IMPORT_HOST_SET_ITEM_CONTEXT));

        // Load all template cells from host into WASM globals.
        // Globals are shared workspace — they hold the LAST item processed.
        // We must load the CURRENT item's persisted values before event handling.
        for info in &all_maps {
            for cell_id in info.cell_range.0..info.cell_range.1 {
                func.instruction(&Instruction::I32Const(cell_id as i32));
                func.instruction(&Instruction::Call(IMPORT_HOST_GET_CELL_F64));
                func.instruction(&Instruction::GlobalSet(cell_id));
            }
        }

        // br_table dispatch on event_id (local 1).
        let num_all_events = self.program.events.len();
        func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty)); // $exit
        for _ in 0..num_all_events {
            func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
        }
        let targets: Vec<u32> = (0..num_all_events as u32).collect();
        let default_target = num_all_events as u32;
        func.instruction(&Instruction::LocalGet(1)); // event_id
        func.instruction(&Instruction::BrTable(targets.into(), default_target));

        for idx in 0..num_all_events {
            func.instruction(&Instruction::End); // end block for event idx
            let event_id = EventId(u32::try_from(idx).unwrap());
            if let Some(mem_ctx) = event_to_mem_ctx.get(&(idx as u32)) {
                self.emit_item_event_handler(&mut func, event_id, mem_ctx);
            }
            let exit_depth = (num_all_events - 1 - idx) as u32;
            func.instruction(&Instruction::Br(exit_depth));
        }

        func.instruction(&Instruction::End); // end $exit

        // Clear item context.
        func.instruction(&Instruction::Call(IMPORT_HOST_CLEAR_ITEM_CONTEXT));
        if has_per_item_remove {
            *self.filter_locals.borrow_mut() = None;
        }
        func.instruction(&Instruction::End);
        func
    }

    /// Emit handler for a per-item event.
    fn emit_item_event_handler(
        &self,
        func: &mut Function,
        event_id: EventId,
        mem_ctx: &MemoryContext,
    ) {
        for node in &self.program.nodes {
            match node {
                IrNode::Then {
                    cell,
                    trigger,
                    body,
                } if *trigger == event_id
                    && cell.0 >= mem_ctx.cell_start
                    && cell.0 < mem_ctx.cell_end =>
                {
                    if self.is_text_body(body) {
                        self.emit_text_setting_ctx(func, *cell, body, Some(mem_ctx));
                    } else {
                        self.emit_expr_ctx(func, body, Some(mem_ctx));
                        self.emit_cell_set(func, *cell, Some(mem_ctx));
                    }
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    self.emit_cell_get(func, *cell, Some(mem_ctx));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    self.emit_item_downstream_updates(func, *cell, mem_ctx);
                }
                IrNode::Hold {
                    cell,
                    trigger_bodies,
                    ..
                } if cell.0 >= mem_ctx.cell_start && cell.0 < mem_ctx.cell_end => {
                    for (trigger, body) in trigger_bodies {
                        if *trigger == event_id {
                            let may_skip = matches!(body, IrExpr::PatternMatch { .. });
                            // Extract text source cell from body for text copy.
                            let text_source = Self::extract_text_source_cell(body);
                            // Re-evaluate the text dependency chain before reading.
                            // The body may reference cells (via CellRead) whose text
                            // was updated by the bridge (e.g., text input's .text cell)
                            // but not re-evaluated through TextTrim/When nodes yet.
                            if let Some(src) = text_source {
                                self.emit_reevaluate_cell_ctx(func, src, mem_ctx);
                            }
                            if may_skip {
                                // PatternMatch may produce SKIP sentinel (specific NaN).
                                // Evaluate, store to temp global, check for SKIP sentinel,
                                // only update cell if NOT SKIP.
                                // Note: We check the exact SKIP sentinel bit pattern
                                // (not just any NaN) because text-only cells have NaN
                                // as their f64 value and should not be treated as SKIP.
                                let skip_global = self.program.cells.len() as u32;
                                self.emit_expr_ctx(func, body, Some(mem_ctx));
                                // Store result to temp global.
                                func.instruction(&Instruction::GlobalSet(skip_global));
                                // Check: is it the SKIP sentinel? Compare bit pattern.
                                func.instruction(&Instruction::GlobalGet(skip_global));
                                func.instruction(&Instruction::I64ReinterpretF64);
                                func.instruction(&Instruction::I64Const(SKIP_SENTINEL_BITS as i64));
                                func.instruction(&Instruction::I64Ne); // true if NOT skip sentinel
                                func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
                                // Not SKIP: load value, store to cell, propagate.
                                func.instruction(&Instruction::GlobalGet(skip_global));
                                self.emit_cell_set(func, *cell, Some(mem_ctx));
                                // Copy text from body result cell to HOLD cell.
                                if let Some(src) = text_source {
                                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                                    func.instruction(&Instruction::I32Const(src.0 as i32));
                                    func.instruction(&Instruction::Call(IMPORT_HOST_COPY_TEXT));
                                }
                                func.instruction(&Instruction::I32Const(cell.0 as i32));
                                self.emit_cell_get(func, *cell, Some(mem_ctx));
                                func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                                if Self::expr_produces_bool(body) {
                                    self.emit_bool_text_update(func, *cell);
                                }
                                self.emit_item_downstream_updates(func, *cell, mem_ctx);
                                func.instruction(&Instruction::End);
                            } else {
                                self.emit_expr_ctx(func, body, Some(mem_ctx));
                                self.emit_cell_set(func, *cell, Some(mem_ctx));
                                // Copy text from body result cell to HOLD cell.
                                if let Some(src) = text_source {
                                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                                    func.instruction(&Instruction::I32Const(src.0 as i32));
                                    func.instruction(&Instruction::Call(IMPORT_HOST_COPY_TEXT));
                                }
                                func.instruction(&Instruction::I32Const(cell.0 as i32));
                                self.emit_cell_get(func, *cell, Some(mem_ctx));
                                func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                                if Self::expr_produces_bool(body) {
                                    self.emit_bool_text_update(func, *cell);
                                }
                                self.emit_item_downstream_updates(func, *cell, mem_ctx);
                            }
                        }
                    }
                }
                IrNode::Latest { target, arms }
                    if target.0 >= mem_ctx.cell_start && target.0 < mem_ctx.cell_end =>
                {
                    for arm in arms {
                        if arm.trigger == Some(event_id) {
                            let may_skip = matches!(&arm.body, IrExpr::PatternMatch { .. });
                            let text_source = Self::extract_text_source_cell(&arm.body);
                            if self.is_text_body(&arm.body) {
                                self.emit_text_setting_ctx(func, *target, &arm.body, Some(mem_ctx));
                                func.instruction(&Instruction::I32Const(target.0 as i32));
                                self.emit_cell_get(func, *target, Some(mem_ctx));
                                func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                                self.emit_item_downstream_updates(func, *target, mem_ctx);
                            } else if may_skip {
                                let skip_global = self.program.cells.len() as u32;
                                self.emit_expr_ctx(func, &arm.body, Some(mem_ctx));
                                func.instruction(&Instruction::GlobalSet(skip_global));
                                func.instruction(&Instruction::GlobalGet(skip_global));
                                func.instruction(&Instruction::I64ReinterpretF64);
                                func.instruction(&Instruction::I64Const(SKIP_SENTINEL_BITS as i64));
                                func.instruction(&Instruction::I64Ne); // true if NOT skip sentinel
                                func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
                                func.instruction(&Instruction::GlobalGet(skip_global));
                                self.emit_cell_set(func, *target, Some(mem_ctx));
                                if let Some(src) = text_source {
                                    func.instruction(&Instruction::I32Const(target.0 as i32));
                                    func.instruction(&Instruction::I32Const(src.0 as i32));
                                    func.instruction(&Instruction::Call(IMPORT_HOST_COPY_TEXT));
                                }
                                func.instruction(&Instruction::I32Const(target.0 as i32));
                                self.emit_cell_get(func, *target, Some(mem_ctx));
                                func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                                self.emit_item_downstream_updates(func, *target, mem_ctx);
                                func.instruction(&Instruction::End);
                            } else {
                                self.emit_expr_ctx(func, &arm.body, Some(mem_ctx));
                                self.emit_cell_set(func, *target, Some(mem_ctx));
                                if let Some(src) = text_source {
                                    func.instruction(&Instruction::I32Const(target.0 as i32));
                                    func.instruction(&Instruction::I32Const(src.0 as i32));
                                    func.instruction(&Instruction::Call(IMPORT_HOST_COPY_TEXT));
                                }
                                func.instruction(&Instruction::I32Const(target.0 as i32));
                                self.emit_cell_get(func, *target, Some(mem_ctx));
                                func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                                self.emit_item_downstream_updates(func, *target, mem_ctx);
                            }
                        }
                    }
                }
                // Per-item ListRemove (Case 1): trigger is template-scoped event.
                // Build a new list excluding the item at item_idx (local 0).
                IrNode::ListRemove {
                    cell,
                    source,
                    trigger,
                    predicate: None,
                    ..
                } if *trigger == event_id => {
                    if let Some((local_new_list, local_count, local_i, _local_mem_idx)) =
                        *self.filter_locals.borrow()
                    {
                        // 1. Create new empty list
                        func.instruction(&Instruction::Call(IMPORT_HOST_LIST_CREATE));
                        func.instruction(&Instruction::LocalSet(local_new_list));

                        // 2. Get item count from source list
                        func.instruction(&Instruction::I32Const(source.0 as i32));
                        func.instruction(&Instruction::Call(IMPORT_HOST_LIST_COUNT));
                        func.instruction(&Instruction::I32TruncF64S);
                        func.instruction(&Instruction::LocalSet(local_count));

                        // 3. Loop: i = 0
                        func.instruction(&Instruction::I32Const(0));
                        func.instruction(&Instruction::LocalSet(local_i));

                        // block $break { loop $continue { ... } }
                        func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
                        func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));

                        // if (i >= count) break
                        func.instruction(&Instruction::LocalGet(local_i));
                        func.instruction(&Instruction::LocalGet(local_count));
                        func.instruction(&Instruction::I32GeS);
                        func.instruction(&Instruction::BrIf(1));

                        // if (memory_index_of(i) != item_idx) → copy item
                        // item_idx (local 0) is a MEMORY index, not a position index.
                        // After removals/reindexing, position != memory index, so we
                        // must look up the memory index for position i before comparing.
                        func.instruction(&Instruction::I32Const(source.0 as i32));
                        func.instruction(&Instruction::LocalGet(local_i));
                        func.instruction(&Instruction::Call(IMPORT_HOST_LIST_ITEM_MEMORY_INDEX));
                        func.instruction(&Instruction::LocalGet(0)); // item_idx (memory index)
                        func.instruction(&Instruction::I32Ne);
                        func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
                        // host_list_copy_item(new_list_id, source_cell_id, i)
                        func.instruction(&Instruction::LocalGet(local_new_list));
                        func.instruction(&Instruction::I32Const(source.0 as i32));
                        func.instruction(&Instruction::LocalGet(local_i));
                        func.instruction(&Instruction::Call(IMPORT_HOST_LIST_COPY_ITEM));
                        func.instruction(&Instruction::End); // end if

                        // i++
                        func.instruction(&Instruction::LocalGet(local_i));
                        func.instruction(&Instruction::I32Const(1));
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::LocalSet(local_i));
                        func.instruction(&Instruction::Br(0));

                        func.instruction(&Instruction::End); // end loop
                        func.instruction(&Instruction::End); // end block

                        // 4. Set ListRemove cell to new (filtered) list
                        func.instruction(&Instruction::LocalGet(local_new_list));
                        func.instruction(&Instruction::GlobalSet(cell.0));
                        func.instruction(&Instruction::I32Const(cell.0 as i32));
                        func.instruction(&Instruction::GlobalGet(cell.0));
                        func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));

                        // 5. Copy filtered result back to source list (in-place modification).
                        // Without this, the source list still contains the removed item.
                        // When a new item is appended to the source list, the removed item
                        // would reappear because the downstream propagation copies the source
                        // list_id directly to the ListRemove cell.
                        func.instruction(&Instruction::I32Const(source.0 as i32));
                        func.instruction(&Instruction::I32Const(cell.0 as i32));
                        func.instruction(&Instruction::Call(IMPORT_HOST_LIST_REPLACE));
                        // Reset remove cell to point to same list as source.
                        func.instruction(&Instruction::GlobalGet(source.0));
                        func.instruction(&Instruction::GlobalSet(cell.0));
                        func.instruction(&Instruction::I32Const(cell.0 as i32));
                        func.instruction(&Instruction::GlobalGet(cell.0));
                        func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));

                        // 6. Emit downstream updates (ListCount, ListMap, ListRetain, etc.)
                        self.emit_list_downstream_updates(func, *cell);
                    }
                }
                _ => {}
            }
        }

        // Emit downstream updates for payload cells (if they're in template range).
        let event_info = &self.program.events[event_id.0 as usize];
        for &payload_cell in &event_info.payload_cells {
            if payload_cell.0 >= mem_ctx.cell_start && payload_cell.0 < mem_ctx.cell_end {
                self.emit_item_downstream_updates(func, payload_cell, mem_ctx);
            }
        }
    }

    /// Emit text setting with optional memory context.
    fn emit_text_setting_ctx(
        &self,
        func: &mut Function,
        cell: CellId,
        body: &IrExpr,
        mem_ctx: Option<&MemoryContext>,
    ) {
        if let IrExpr::Constant(IrValue::Text(t)) = body {
            // Constant text (e.g., Text/empty(), Text/space()).
            let pattern_idx = self.register_text_pattern(t);
            func.instruction(&Instruction::I32Const(cell.0 as i32));
            func.instruction(&Instruction::I32Const(pattern_idx as i32));
            func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_TEXT_PATTERN));
            // Bump cell value so signal fires.
            self.emit_cell_get(func, cell, mem_ctx);
            func.instruction(&Instruction::F64Const(1.0));
            func.instruction(&Instruction::F64Add);
            self.emit_cell_set(func, cell, mem_ctx);
        } else if let IrExpr::TextConcat(segments) = body {
            let mut all_literal = true;
            let mut text = String::new();
            for seg in segments {
                match seg {
                    TextSegment::Literal(s) => text.push_str(s),
                    _ => {
                        all_literal = false;
                        break;
                    }
                }
            }
            if all_literal {
                let pattern_idx = self.register_text_pattern(&text);
                func.instruction(&Instruction::I32Const(cell.0 as i32));
                func.instruction(&Instruction::I32Const(pattern_idx as i32));
                func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_TEXT_PATTERN));
                // Bump cell value so signal fires.
                self.emit_cell_get(func, cell, mem_ctx);
                func.instruction(&Instruction::F64Const(1.0));
                func.instruction(&Instruction::F64Add);
                self.emit_cell_set(func, cell, mem_ctx);
            } else {
                self.emit_text_build_ctx(func, cell, segments, mem_ctx);
            }
        } else if let IrExpr::CellRead(source) = body {
            func.instruction(&Instruction::I32Const(cell.0 as i32));
            func.instruction(&Instruction::I32Const(source.0 as i32));
            func.instruction(&Instruction::Call(IMPORT_HOST_COPY_TEXT));
        }
    }

    /// Emit text build with optional memory context.
    fn emit_text_build_ctx(
        &self,
        func: &mut Function,
        target: CellId,
        segments: &[TextSegment],
        mem_ctx: Option<&MemoryContext>,
    ) {
        func.instruction(&Instruction::I32Const(target.0 as i32));
        func.instruction(&Instruction::Call(IMPORT_HOST_TEXT_BUILD_START));
        for seg in segments {
            match seg {
                TextSegment::Literal(s) => {
                    let pattern_idx = self.register_text_pattern(s);
                    func.instruction(&Instruction::I32Const(pattern_idx as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_TEXT_BUILD_LITERAL));
                }
                TextSegment::Expr(IrExpr::CellRead(cell)) => {
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_TEXT_BUILD_CELL));
                }
                TextSegment::Expr(_) => {
                    let pattern_idx = self.register_text_pattern("?");
                    func.instruction(&Instruction::I32Const(pattern_idx as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_TEXT_BUILD_LITERAL));
                }
            }
        }
        // Bump cell value so signal fires.
        self.emit_cell_get(func, target, mem_ctx);
        func.instruction(&Instruction::F64Const(1.0));
        func.instruction(&Instruction::F64Add);
        self.emit_cell_set(func, target, mem_ctx);
    }

    /// Emit pattern match with optional memory context.
    fn emit_pattern_match_ctx(
        &self,
        func: &mut Function,
        source: CellId,
        arms: &[(IrPattern, IrExpr)],
        target: CellId,
        mem_ctx: Option<&MemoryContext>,
        propagate_downstream: bool,
    ) {
        self.emit_pattern_arms_ctx(func, source, arms, target, 0, mem_ctx, propagate_downstream);
    }

    fn emit_pattern_arms_ctx(
        &self,
        func: &mut Function,
        source: CellId,
        arms: &[(IrPattern, IrExpr)],
        target: CellId,
        idx: usize,
        mem_ctx: Option<&MemoryContext>,
        propagate_downstream: bool,
    ) {
        if idx >= arms.len() {
            return;
        }
        let (pattern, body) = &arms[idx];
        let is_skip = matches!(body, IrExpr::Constant(IrValue::Skip));
        let has_more = idx + 1 < arms.len();

        match pattern {
            IrPattern::Tag(tag) => {
                let encoded = self
                    .program
                    .tag_table
                    .iter()
                    .position(|t| t == tag)
                    .map(|i| (i + 1) as f64)
                    .unwrap_or(0.0);
                self.emit_cell_get(func, source, mem_ctx);
                func.instruction(&Instruction::F64Const(encoded));
                func.instruction(&Instruction::F64Eq);
                func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
                if !is_skip {
                    self.emit_arm_body_ctx(func, body, target, mem_ctx, propagate_downstream);
                }
                if has_more {
                    func.instruction(&Instruction::Else);
                    self.emit_pattern_arms_ctx(func, source, arms, target, idx + 1, mem_ctx, propagate_downstream);
                }
                func.instruction(&Instruction::End);
            }
            IrPattern::Number(n) => {
                self.emit_cell_get(func, source, mem_ctx);
                func.instruction(&Instruction::F64Const(*n));
                func.instruction(&Instruction::F64Eq);
                func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
                if !is_skip {
                    self.emit_arm_body_ctx(func, body, target, mem_ctx, propagate_downstream);
                }
                if has_more {
                    func.instruction(&Instruction::Else);
                    self.emit_pattern_arms_ctx(func, source, arms, target, idx + 1, mem_ctx, propagate_downstream);
                }
                func.instruction(&Instruction::End);
            }
            IrPattern::Text(text) => {
                let pattern_idx = self.register_text_pattern(text);
                let text_source = self.resolve_text_cell(source);
                func.instruction(&Instruction::I32Const(text_source.0 as i32));
                func.instruction(&Instruction::I32Const(pattern_idx as i32));
                func.instruction(&Instruction::Call(IMPORT_HOST_TEXT_MATCHES));
                func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
                if !is_skip {
                    self.emit_arm_body_ctx(func, body, target, mem_ctx, propagate_downstream);
                }
                if has_more {
                    func.instruction(&Instruction::Else);
                    self.emit_pattern_arms_ctx(func, source, arms, target, idx + 1, mem_ctx, propagate_downstream);
                }
                func.instruction(&Instruction::End);
            }
            IrPattern::Wildcard | IrPattern::Binding(_) => {
                if !is_skip {
                    self.emit_arm_body_ctx(func, body, target, mem_ctx, propagate_downstream);
                }
            }
        }
    }

    /// Emit arm body with optional memory context.
    fn emit_arm_body_ctx(
        &self,
        func: &mut Function,
        body: &IrExpr,
        target: CellId,
        mem_ctx: Option<&MemoryContext>,
        propagate_downstream: bool,
    ) {
        if self.is_text_body(body) {
            self.emit_text_setting_ctx(func, target, body, mem_ctx);
        } else {
            self.emit_expr_ctx(func, body, mem_ctx);
            self.emit_cell_set(func, target, mem_ctx);
            if let IrExpr::CellRead(src) = body {
                func.instruction(&Instruction::I32Const(target.0 as i32));
                func.instruction(&Instruction::I32Const(src.0 as i32));
                func.instruction(&Instruction::Call(IMPORT_HOST_COPY_TEXT));
            }
        }
        func.instruction(&Instruction::I32Const(target.0 as i32));
        self.emit_cell_get(func, target, mem_ctx);
        func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
        if propagate_downstream {
            if let Some(ctx) = mem_ctx {
                self.emit_item_downstream_updates(func, target, ctx);
            } else {
                self.emit_downstream_updates(func, target);
            }
        }
    }

    /// Emit downstream updates for template-scoped cells only.
    /// Only walks nodes in the template range and uses MemoryContext for access.
    fn emit_item_downstream_updates(
        &self,
        func: &mut Function,
        updated_cell: CellId,
        mem_ctx: &MemoryContext,
    ) {
        for node in &self.program.nodes {
            match node {
                IrNode::PipeThrough { cell, source }
                    if *source == updated_cell
                        && cell.0 >= mem_ctx.cell_start
                        && cell.0 < mem_ctx.cell_end =>
                {
                    self.emit_cell_get(func, updated_cell, Some(mem_ctx));
                    self.emit_cell_set(func, *cell, Some(mem_ctx));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    self.emit_cell_get(func, *cell, Some(mem_ctx));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::I32Const(updated_cell.0 as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_COPY_TEXT));
                    self.emit_item_downstream_updates(func, *cell, mem_ctx);
                }
                IrNode::Derived {
                    cell,
                    expr: IrExpr::CellRead(source),
                } if *source == updated_cell
                    && cell.0 >= mem_ctx.cell_start
                    && cell.0 < mem_ctx.cell_end =>
                {
                    self.emit_cell_get(func, updated_cell, Some(mem_ctx));
                    self.emit_cell_set(func, *cell, Some(mem_ctx));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    self.emit_cell_get(func, *cell, Some(mem_ctx));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::I32Const(updated_cell.0 as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_COPY_TEXT));
                    self.emit_item_downstream_updates(func, *cell, mem_ctx);
                }
                IrNode::Derived { cell, expr }
                    if !matches!(expr, IrExpr::CellRead(_))
                        && Self::expr_references_cell(expr, updated_cell)
                        && cell.0 >= mem_ctx.cell_start
                        && cell.0 < mem_ctx.cell_end =>
                {
                    if let IrExpr::TextConcat(segments) = expr {
                        self.emit_text_build_ctx(func, *cell, segments, Some(mem_ctx));
                    } else {
                        self.emit_expr_ctx(func, expr, Some(mem_ctx));
                        self.emit_cell_set(func, *cell, Some(mem_ctx));
                    }
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    self.emit_cell_get(func, *cell, Some(mem_ctx));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    self.emit_item_downstream_updates(func, *cell, mem_ctx);
                }
                IrNode::When { cell, source, arms }
                    if *source == updated_cell
                        && cell.0 >= mem_ctx.cell_start
                        && cell.0 < mem_ctx.cell_end =>
                {
                    self.emit_pattern_match_ctx(func, *source, arms, *cell, Some(mem_ctx), true);
                }
                IrNode::While {
                    cell,
                    source,
                    deps,
                    arms,
                } if (*source == updated_cell || deps.contains(&updated_cell))
                    && cell.0 >= mem_ctx.cell_start
                    && cell.0 < mem_ctx.cell_end =>
                {
                    self.emit_pattern_match_ctx(func, *source, arms, *cell, Some(mem_ctx), true);
                }
                IrNode::Latest { target, arms }
                    if *target != updated_cell
                        && target.0 >= mem_ctx.cell_start
                        && target.0 < mem_ctx.cell_end =>
                {
                    if let Some(arm) = arms.iter().find(|arm| {
                        arm.trigger.is_none() && Self::expr_references_cell(&arm.body, updated_cell)
                    }) {
                        let may_skip = matches!(&arm.body, IrExpr::PatternMatch { .. });
                        let text_source = Self::extract_text_source_cell(&arm.body);
                        if self.is_text_body(&arm.body) {
                            self.emit_text_setting_ctx(func, *target, &arm.body, Some(mem_ctx));
                            func.instruction(&Instruction::I32Const(target.0 as i32));
                            self.emit_cell_get(func, *target, Some(mem_ctx));
                            func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                            self.emit_item_downstream_updates(func, *target, mem_ctx);
                        } else if may_skip {
                            let skip_global = self.program.cells.len() as u32;
                            self.emit_expr_ctx(func, &arm.body, Some(mem_ctx));
                            func.instruction(&Instruction::GlobalSet(skip_global));
                            func.instruction(&Instruction::GlobalGet(skip_global));
                            func.instruction(&Instruction::I64ReinterpretF64);
                            func.instruction(&Instruction::I64Const(SKIP_SENTINEL_BITS as i64));
                            func.instruction(&Instruction::I64Ne); // true if NOT skip sentinel
                            func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
                            func.instruction(&Instruction::GlobalGet(skip_global));
                            self.emit_cell_set(func, *target, Some(mem_ctx));
                            if let Some(src) = text_source {
                                func.instruction(&Instruction::I32Const(target.0 as i32));
                                func.instruction(&Instruction::I32Const(src.0 as i32));
                                func.instruction(&Instruction::Call(IMPORT_HOST_COPY_TEXT));
                            }
                            func.instruction(&Instruction::I32Const(target.0 as i32));
                            self.emit_cell_get(func, *target, Some(mem_ctx));
                            func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                            self.emit_item_downstream_updates(func, *target, mem_ctx);
                            func.instruction(&Instruction::End);
                        } else {
                            self.emit_expr_ctx(func, &arm.body, Some(mem_ctx));
                            self.emit_cell_set(func, *target, Some(mem_ctx));
                            if let Some(src) = text_source {
                                func.instruction(&Instruction::I32Const(target.0 as i32));
                                func.instruction(&Instruction::I32Const(src.0 as i32));
                                func.instruction(&Instruction::Call(IMPORT_HOST_COPY_TEXT));
                            }
                            func.instruction(&Instruction::I32Const(target.0 as i32));
                            self.emit_cell_get(func, *target, Some(mem_ctx));
                            func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                            self.emit_item_downstream_updates(func, *target, mem_ctx);
                        }
                    }
                }
                IrNode::TextIsNotEmpty { cell, source }
                    if *source == updated_cell
                        && cell.0 >= mem_ctx.cell_start
                        && cell.0 < mem_ctx.cell_end =>
                {
                    func.instruction(&Instruction::I32Const(source.0 as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_TEXT_IS_NOT_EMPTY));
                    self.emit_cell_set(func, *cell, Some(mem_ctx));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    self.emit_cell_get(func, *cell, Some(mem_ctx));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    self.emit_item_downstream_updates(func, *cell, mem_ctx);
                }
                IrNode::TextTrim { cell, source }
                    if *source == updated_cell
                        && cell.0 >= mem_ctx.cell_start
                        && cell.0 < mem_ctx.cell_end =>
                {
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::I32Const(source.0 as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_TEXT_TRIM));
                    self.emit_cell_get(func, updated_cell, Some(mem_ctx));
                    self.emit_cell_set(func, *cell, Some(mem_ctx));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    self.emit_cell_get(func, *cell, Some(mem_ctx));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    self.emit_item_downstream_updates(func, *cell, mem_ctx);
                }
                _ => {}
            }
        }
    }

    /// Emit `get_item_cell(item_idx: i32, cell_id: i32) -> f64`.
    /// Reads a per-item cell value via host import (globals-as-workspace pattern).
    fn emit_get_item_cell(&self) -> Function {
        let mut func = Function::new([]);

        // Set item context so host_get_cell_f64 reads from the correct item.
        func.instruction(&Instruction::LocalGet(0)); // item_idx
        func.instruction(&Instruction::Call(IMPORT_HOST_SET_ITEM_CONTEXT));

        // Read via host import.
        func.instruction(&Instruction::LocalGet(1)); // cell_id
        func.instruction(&Instruction::Call(IMPORT_HOST_GET_CELL_F64));

        // Clear item context.
        func.instruction(&Instruction::Call(IMPORT_HOST_CLEAR_ITEM_CONTEXT));

        func.instruction(&Instruction::End);
        func
    }

    /// Emit `rerun_retain_filters()` — re-evaluates all per-item retain filter loops.
    /// Called from the host after cross-scope events update per-item cells,
    /// so global retain filters see the new per-item values.
    fn emit_rerun_retain_filters(&self) -> Function {
        // Check if we have any per-item retains.
        if !self.has_per_item_filter() {
            let mut func = Function::new([]);
            func.instruction(&Instruction::End);
            return func;
        }

        // Locals: 0=f64 (loop var), 1=i32 (counter), 2=i32 (count), 3=i32 (mem_idx)
        let mut func = Function::new([(1, ValType::F64), (3, ValType::I32)]);

        // Set filter_locals for the filter loop helper.
        *self.filter_locals.borrow_mut() = Some((0, 1, 2, 3));

        for node in &self.program.nodes {
            match node {
                IrNode::ListRetain {
                    cell,
                    source,
                    predicate: Some(pred),
                    item_cell,
                    item_field_cells,
                } if item_cell.is_some() => {
                    self.emit_filter_loop(
                        &mut func,
                        *cell,
                        *source,
                        *pred,
                        *item_cell,
                        item_field_cells,
                        0,
                        1,
                        2,
                        3,
                        false,
                    );
                    self.emit_list_downstream_updates(&mut func, *cell);
                }
                IrNode::ListEvery {
                    cell,
                    source,
                    predicate: Some(pred),
                    item_cell,
                    item_field_cells,
                } if item_cell.is_some() => {
                    self.emit_boolean_check_loop(
                        &mut func, *cell, *source, *pred, *item_cell,
                        item_field_cells, 0, 1, 2, 3, true,
                    );
                    self.emit_downstream_updates(&mut func, *cell);
                }
                IrNode::ListAny {
                    cell,
                    source,
                    predicate: Some(pred),
                    item_cell,
                    item_field_cells,
                } if item_cell.is_some() => {
                    self.emit_boolean_check_loop(
                        &mut func, *cell, *source, *pred, *item_cell,
                        item_field_cells, 0, 1, 2, 3, false,
                    );
                    self.emit_downstream_updates(&mut func, *cell);
                }
                IrNode::ListRemove { .. } => {}
                _ => {}
            }
        }

        func.instruction(&Instruction::End);
        func
    }
}
