//! IR → WASM binary code generation using wasm-encoder.
//!
//! Generates a WASM module with:
//! - One mutable f64 global per cell (cell values)
//! - Host imports for DOM updates: host_set_cell_f64, host_set_cell_text, host_log
//! - `init()` — sets initial cell values, calls host emit for initial render
//! - `on_event(event_id: i32)` — dispatches events, updates cells, re-emits

use std::cell::RefCell;

use wasm_encoder::{
    CodeSection, ConstExpr, ExportKind, ExportSection, Function, FunctionSection, GlobalSection,
    GlobalType, ImportSection, Instruction, MemorySection, MemoryType, Module, TypeSection, ValType,
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

const NUM_IMPORTS: u32 = 18;

// Exported function indices (offset by NUM_IMPORTS)
const FN_INIT: u32 = NUM_IMPORTS;
const FN_ON_EVENT: u32 = NUM_IMPORTS + 1;
const FN_SET_GLOBAL: u32 = NUM_IMPORTS + 2;
const FN_INIT_ITEM: u32 = NUM_IMPORTS + 3;
const FN_ON_ITEM_EVENT: u32 = NUM_IMPORTS + 4;
const FN_GET_ITEM_CELL: u32 = NUM_IMPORTS + 5;

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

/// Context for memory-based per-item cell access.
/// When present, template-scoped cells use WASM linear memory
/// instead of globals.
struct MemoryContext {
    /// WASM local index holding item_idx (i32).
    item_idx_local: u32,
    /// Base address in linear memory for per-item data.
    memory_base: u32,
    /// Bytes per item (template_cell_count * 8).
    stride: u32,
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
    /// Local indices for the per-item retain filter loop: (new_list_id, count, i).
    /// Set by emit_init / emit_on_event before emitting code that may trigger
    /// the filter loop via emit_downstream_updates.
    filter_locals: RefCell<Option<(u32, u32, u32)>>,
}

impl<'a> WasmEmitter<'a> {
    fn new(program: &'a IrProgram) -> Self {
        Self {
            program,
            text_patterns: RefCell::new(Vec::new()),
            filter_locals: RefCell::new(None),
        }
    }

    /// Register a text pattern and return its index.
    /// Follow Derived CellRead chains to find the cell that actually holds text.
    /// Used for text pattern matching where an intermediate cell is a pass-through.
    fn resolve_text_cell(&self, cell: CellId) -> CellId {
        for node in &self.program.nodes {
            if let IrNode::Derived { cell: c, expr: IrExpr::CellRead(target) } = node {
                if *c == cell {
                    return self.resolve_text_cell(*target);
                }
            }
        }
        cell
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
        types.ty().function([ValType::I32, ValType::I32], [ValType::I32]);
        // Type 10: (i32, i32) -> f64 [get_item_cell]
        types.ty().function([ValType::I32, ValType::I32], [ValType::F64]);
        // Type 11: (f64, i32, i32) -> () [host_list_copy_item]
        types.ty().function([ValType::F64, ValType::I32, ValType::I32], []);
        module.section(&types);

        // 2. Import section
        let mut imports = ImportSection::new();
        imports.import("env", "host_set_cell_f64", wasm_encoder::EntityType::Function(0));
        imports.import("env", "host_notify_init_done", wasm_encoder::EntityType::Function(1));
        imports.import("env", "host_list_create", wasm_encoder::EntityType::Function(2));
        imports.import("env", "host_list_append", wasm_encoder::EntityType::Function(3));
        imports.import("env", "host_list_clear", wasm_encoder::EntityType::Function(4));
        imports.import("env", "host_list_count", wasm_encoder::EntityType::Function(5));
        imports.import("env", "host_text_trim", wasm_encoder::EntityType::Function(8));
        imports.import("env", "host_text_is_not_empty", wasm_encoder::EntityType::Function(5));
        imports.import("env", "host_copy_text", wasm_encoder::EntityType::Function(8));
        imports.import("env", "host_list_append_text", wasm_encoder::EntityType::Function(8));
        imports.import("env", "host_text_matches", wasm_encoder::EntityType::Function(9));
        imports.import("env", "host_set_cell_text_pattern", wasm_encoder::EntityType::Function(8));
        imports.import("env", "host_text_build_start", wasm_encoder::EntityType::Function(4));
        imports.import("env", "host_text_build_literal", wasm_encoder::EntityType::Function(4));
        imports.import("env", "host_text_build_cell", wasm_encoder::EntityType::Function(4));
        imports.import("env", "host_set_item_context", wasm_encoder::EntityType::Function(4));
        imports.import("env", "host_clear_item_context", wasm_encoder::EntityType::Function(1));
        imports.import("env", "host_list_copy_item", wasm_encoder::EntityType::Function(11));
        module.section(&imports);

        // 3. Function section (declares init, on_event, set_global, init_item, on_item_event, get_item_cell)
        let mut functions = FunctionSection::new();
        functions.function(6);  // init: () -> ()
        functions.function(7);  // on_event: (i32) -> ()
        functions.function(0);  // set_global: (i32, f64) -> ()
        functions.function(7);  // init_item: (i32) -> ()
        functions.function(8);  // on_item_event: (i32, i32) -> ()
        functions.function(10); // get_item_cell: (i32, i32) -> f64
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
        module.section(&exports);

        // 7. Code section
        let mut code = CodeSection::new();

        // init() body
        let init_func = self.emit_init();
        code.function(&init_func);

        // on_event(event_id) body
        let on_event_func = self.emit_on_event();
        code.function(&on_event_func);

        // set_global(cell_id: i32, value: f64) body
        let set_global_func = self.emit_set_global();
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

        module.section(&code);

        module.finish()
    }

    /// Emit the `init()` function body.
    /// Sets initial cell values and calls host_set_cell_f64 for each cell.
    /// Check if a cell is used as source for any list operation node.
    fn is_list_source(&self, cell: CellId) -> bool {
        self.program.nodes.iter().any(|node| {
            matches!(node,
                IrNode::ListAppend { source, .. }
                | IrNode::ListClear { source, .. }
                | IrNode::ListRemove { source, .. }
                | IrNode::ListRetain { source, .. }
                | IrNode::ListCount { source, .. }
                | IrNode::ListIsEmpty { source, .. }
                | IrNode::ListMap { source, .. }
                if *source == cell
            )
        })
    }

    fn emit_init(&self) -> Function {
        // Count locals needed for HoldLoop nodes (1 counter + N field temps).
        let mut num_hold_loop_locals: u32 = 0;
        for node in &self.program.nodes {
            if let IrNode::HoldLoop { field_cells, .. } = node {
                num_hold_loop_locals = num_hold_loop_locals.max(1 + field_cells.len() as u32);
            }
        }
        let has_filter = self.has_per_item_retain();
        // f64 locals: HoldLoop locals + 1 filter local (new_list_id)
        let num_f64_locals = num_hold_loop_locals + if has_filter { 1 } else { 0 };
        let mut locals: Vec<(u32, ValType)> = Vec::new();
        if num_f64_locals > 0 {
            locals.push((num_f64_locals, ValType::F64));
        }
        if has_filter {
            locals.push((2, ValType::I32)); // count, i
        }
        let mut func = Function::new(locals);

        // Phase 1: Initialize all WASM globals to default values.
        // This must happen before Phase 2 because downstream updates (e.g., MathSum
        // accumulation) read from globals that might not be initialized yet.
        for node in &self.program.nodes {
            match node {
                IrNode::MathSum { cell, .. } => {
                    func.instruction(&Instruction::F64Const(0.0));
                    func.instruction(&Instruction::GlobalSet(cell.0));
                }
                IrNode::Hold { cell, init, .. } => {
                    self.emit_expr(&mut func, init);
                    func.instruction(&Instruction::GlobalSet(cell.0));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    // Copy text from init expression (e.g., CellRead from a text cell).
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
                _ => {}
            }
        }

        // Set filter loop locals for init context so emit_downstream_updates can use them.
        if has_filter {
            let local_new_list = num_hold_loop_locals;
            let local_count = num_f64_locals;
            let local_i = num_f64_locals + 1;
            *self.filter_locals.borrow_mut() = Some((local_new_list, local_count, local_i));
        }

        // Phase 2: Evaluate all nodes that may trigger downstream updates.
        for node in &self.program.nodes {
            match node {
                // Skip MathSum and Hold — already initialized in Phase 1.
                IrNode::MathSum { .. } | IrNode::Hold { .. } => {}

                IrNode::HoldLoop { cell: _, field_cells, init_values, count_expr, body_fields } => {
                    // 1. Set field cell globals to initial values.
                    for ((_name, field_cell), (_init_name, init_expr)) in field_cells.iter().zip(init_values.iter()) {
                        self.emit_expr(&mut func, init_expr);
                        func.instruction(&Instruction::GlobalSet(field_cell.0));
                    }

                    // 2. Evaluate loop count and store in local 0.
                    self.emit_expr(&mut func, count_expr);
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
                        self.emit_expr(&mut func, body_expr);
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
                        self.emit_expr(&mut func, expr);
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
                                    IrExpr::CellRead(item_cell) if self.is_namespace_cell(*item_cell) => {
                                        // Namespace cell (object): resolve text from field cells.
                                        let ns_text = self.resolve_namespace_text_statically(*item_cell);
                                        if let Some(text) = ns_text {
                                            // Set text on the item cell, then append.
                                            let pattern_idx = self.register_text_pattern(&text);
                                            func.instruction(&Instruction::I32Const(item_cell.0 as i32));
                                            func.instruction(&Instruction::I32Const(pattern_idx as i32));
                                            func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_TEXT_PATTERN));
                                            func.instruction(&Instruction::I32Const(cell.0 as i32));
                                            func.instruction(&Instruction::I32Const(item_cell.0 as i32));
                                            func.instruction(&Instruction::Call(IMPORT_HOST_LIST_APPEND_TEXT));
                                        } else {
                                            // Fallback: append as-is (empty text).
                                            func.instruction(&Instruction::I32Const(cell.0 as i32));
                                            func.instruction(&Instruction::I32Const(item_cell.0 as i32));
                                            func.instruction(&Instruction::Call(IMPORT_HOST_LIST_APPEND_TEXT));
                                        }
                                    }
                                    IrExpr::CellRead(item_cell) => {
                                        // host_list_append_text(list_cell_id, item_cell_id)
                                        func.instruction(&Instruction::I32Const(cell.0 as i32));
                                        func.instruction(&Instruction::I32Const(item_cell.0 as i32));
                                        func.instruction(&Instruction::Call(IMPORT_HOST_LIST_APPEND_TEXT));
                                    }
                                    IrExpr::TextConcat(segments) => {
                                        // Build text on the list cell, then append from it.
                                        // Use the list cell as a temp text buffer (its text
                                        // doesn't matter since the cell stores a list ID as f64).
                                        self.emit_text_build(&mut func, *cell, segments);
                                        func.instruction(&Instruction::I32Const(cell.0 as i32));
                                        func.instruction(&Instruction::I32Const(cell.0 as i32));
                                        func.instruction(&Instruction::Call(IMPORT_HOST_LIST_APPEND_TEXT));
                                    }
                                    _ => {
                                        // host_list_append(list_cell_id, value)
                                        func.instruction(&Instruction::I32Const(cell.0 as i32));
                                        self.emit_expr(&mut func, item);
                                        func.instruction(&Instruction::Call(IMPORT_HOST_LIST_APPEND));
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
                            self.emit_expr(&mut func, &arm.body);
                            func.instruction(&Instruction::GlobalSet(target.0));
                            func.instruction(&Instruction::I32Const(target.0 as i32));
                            func.instruction(&Instruction::GlobalGet(target.0));
                            func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                            // Propagate downstream (e.g., to MathSum).
                            self.emit_downstream_updates(&mut func, *target);
                            break;
                        }
                    }
                }
                IrNode::When { cell, source, arms } => {
                    // Evaluate initial value by pattern matching on source cell.
                    self.emit_pattern_match(&mut func, *source, arms, *cell);
                }
                IrNode::While { cell, source, arms, .. } => {
                    // Same as WHEN for initial evaluation.
                    self.emit_pattern_match(&mut func, *source, arms, *cell);
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
                    // Pass list ID through, like ListAppend.
                    func.instruction(&Instruction::GlobalGet(source.0));
                    func.instruction(&Instruction::GlobalSet(cell.0));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                }
                IrNode::ListRetain { cell, source, predicate, item_field_cells, .. } => {
                    if !item_field_cells.is_empty() {
                        // Per-item filtering: run filter loop using saved locals.
                        if let (Some(pred), Some((l0, l1, l2))) = (predicate, *self.filter_locals.borrow()) {
                            self.emit_retain_filter_loop(
                                &mut func, *cell, *source, *pred,
                                item_field_cells, l0, l1, l2,
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
                IrNode::ListIsEmpty { cell, source } => {
                    // Check if count == 0 → 1.0 (True), else 0.0 (False).
                    func.instruction(&Instruction::I32Const(source.0 as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_LIST_COUNT));
                    func.instruction(&Instruction::F64Const(0.0));
                    func.instruction(&Instruction::F64Eq);
                    func.instruction(&Instruction::If(wasm_encoder::BlockType::Result(ValType::F64)));
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

        // Signal that init is complete.
        func.instruction(&Instruction::Call(IMPORT_HOST_NOTIFY_INIT_DONE));
        func.instruction(&Instruction::End);
        *self.filter_locals.borrow_mut() = None;
        func
    }

    /// Emit the `on_event(event_id: i32)` function body.
    /// Uses `br_table` for O(1) dispatch when there are multiple events.
    fn emit_on_event(&self) -> Function {
        // on_event has param local 0 (event_id: i32).
        // Add filter loop locals if needed.
        let has_filter = self.has_per_item_retain();
        let locals: Vec<(u32, ValType)> = if has_filter {
            vec![(1, ValType::F64), (2, ValType::I32)] // new_list_id (f64), count + i (i32 x2)
        } else {
            vec![]
        };
        if has_filter {
            // on_event locals: param 0=event_id(i32), local 1=new_list(f64), local 2=count(i32), local 3=i(i32)
            *self.filter_locals.borrow_mut() = Some((1, 2, 3));
        }
        let mut func = Function::new(locals);
        let num_events = self.program.events.len();

        if num_events == 0 {
            func.instruction(&Instruction::End);
            return func;
        }

        // Use br_table for O(1) dispatch.
        // Structure: block $exit { block $0 { block $1 { ... br_table } handler_N-1 } ... handler_0 }
        // br_table targets are innermost-first: index 0 → $0 (innermost),
        // which falls through to handler_0 code after its block end.

        // Open outer block ($exit) — br to here skips everything.
        func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));

        // Open one nested block per event (innermost = event 0).
        for _ in 0..num_events {
            func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
        }

        // br_table: targets[i] = branch depth for event i.
        // Event 0 → depth 0 (breaks innermost block, lands at handler 0 code).
        // Event 1 → depth 1, etc.
        // Default (out of range) → depth num_events (breaks to $exit).
        let targets: Vec<u32> = (0..num_events as u32).collect();
        let default_target = num_events as u32; // $exit
        func.instruction(&Instruction::LocalGet(0));
        func.instruction(&Instruction::BrTable(targets.into(), default_target));

        // Emit handlers in order: event 0 first (innermost block ends first).
        for idx in 0..num_events {
            func.instruction(&Instruction::End); // end block for event idx
            let event_id = EventId(u32::try_from(idx).unwrap());
            self.emit_event_handler(&mut func, event_id);
            // Branch to $exit after handling.
            let exit_depth = (num_events - 1 - idx) as u32;
            func.instruction(&Instruction::Br(exit_depth));
        }

        func.instruction(&Instruction::End); // end $exit block
        func.instruction(&Instruction::End); // end function
        *self.filter_locals.borrow_mut() = None;
        func
    }

    /// Emit the `set_global(cell_id: i32, value: f64)` function body.
    /// Uses `br_table` for O(1) dispatch.
    fn emit_set_global(&self) -> Function {
        let mut func = Function::new([]);
        let num_cells = self.program.cells.len();

        if num_cells == 0 {
            func.instruction(&Instruction::End);
            return func;
        }

        // Same br_table pattern as on_event.
        func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
        for _ in 0..num_cells {
            func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
        }

        let targets: Vec<u32> = (0..num_cells as u32).collect();
        let default_target = num_cells as u32;
        func.instruction(&Instruction::LocalGet(0)); // cell_id
        func.instruction(&Instruction::BrTable(targets.into(), default_target));

        for idx in 0..num_cells {
            func.instruction(&Instruction::End); // end block for cell idx
            func.instruction(&Instruction::LocalGet(1)); // value
            func.instruction(&Instruction::GlobalSet(u32::try_from(idx).unwrap()));
            let exit_depth = (num_cells - 1 - idx) as u32;
            func.instruction(&Instruction::Br(exit_depth));
        }

        func.instruction(&Instruction::End); // end $exit
        func.instruction(&Instruction::End); // end function
        func
    }

    /// Emit handler code for a specific event.
    fn emit_event_handler(&self, func: &mut Function, event_id: EventId) {
        for node in &self.program.nodes {
            match node {
                IrNode::Then { cell, trigger, body } if *trigger == event_id => {
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
                IrNode::Hold { cell, trigger_bodies, .. } => {
                    for (trigger, body) in trigger_bodies {
                        if *trigger == event_id {
                            // Evaluate body (which reads current state via GlobalGet).
                            self.emit_expr(func, body);
                            func.instruction(&Instruction::GlobalSet(cell.0));
                            // Notify host.
                            func.instruction(&Instruction::I32Const(cell.0 as i32));
                            func.instruction(&Instruction::GlobalGet(cell.0));
                            func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                            // Propagate to downstream nodes (e.g., PipeThrough, MathSum).
                            self.emit_downstream_updates(func, *cell);
                        }
                    }
                }
                IrNode::Latest { target, arms } => {
                    for arm in arms {
                        if arm.trigger == Some(event_id) {
                            if self.is_text_body(&arm.body) {
                                // Text body: set text first, then bump counter.
                                self.emit_text_setting(func, *target, &arm.body);
                            } else {
                                self.emit_expr(func, &arm.body);
                                func.instruction(&Instruction::GlobalSet(target.0));
                            }
                            // Notify host of the target cell update.
                            func.instruction(&Instruction::I32Const(target.0 as i32));
                            func.instruction(&Instruction::GlobalGet(target.0));
                            func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                            self.emit_downstream_updates(func, *target);
                        }
                    }
                }
                IrNode::ListAppend { cell, source, item, trigger, .. } if *trigger == event_id => {
                    if self.is_namespace_cell(*item) {
                        // Namespace cell (object): find the text source and append text.
                        if let Some(text_source) = self.find_text_source_for_namespace(*item) {
                            // Re-evaluate the text source chain to get current text.
                            self.emit_reevaluate_cell(func, text_source);
                            // Append text from the text source cell.
                            func.instruction(&Instruction::I32Const(source.0 as i32));
                            func.instruction(&Instruction::I32Const(text_source.0 as i32));
                            func.instruction(&Instruction::Call(IMPORT_HOST_LIST_APPEND_TEXT));
                        } else {
                            // Fallback: append f64 value.
                            func.instruction(&Instruction::I32Const(source.0 as i32));
                            func.instruction(&Instruction::GlobalGet(item.0));
                            func.instruction(&Instruction::Call(IMPORT_HOST_LIST_APPEND));
                        }
                    } else {
                        // Append: call host_list_append(list_cell_id, item_value)
                        func.instruction(&Instruction::I32Const(source.0 as i32));
                        func.instruction(&Instruction::GlobalGet(item.0));
                        func.instruction(&Instruction::Call(IMPORT_HOST_LIST_APPEND));
                    }
                    // Update downstream (ListCount, ListMap cells).
                    self.emit_list_downstream_updates(func, *cell);
                }
                IrNode::ListClear { cell, source, trigger } if *trigger == event_id => {
                    // Clear: call host_list_clear(list_cell_id)
                    func.instruction(&Instruction::I32Const(source.0 as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_LIST_CLEAR));
                    // Update downstream.
                    self.emit_list_downstream_updates(func, *cell);
                }
                IrNode::ListRemove { cell, trigger, .. } if *trigger == event_id => {
                    // Remove is handled host-side; just trigger downstream updates.
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
        matches!(body, IrExpr::TextConcat(_))
    }

    /// If `body` is a TextConcat with all-literal segments, emit a call to
    /// `host_set_cell_text_pattern` so the host sets the cell's text content.
    /// Also bumps the cell's f64 global by +1 so signal watchers fire even
    /// when the numeric value would otherwise stay constant (0.0 → 0.0).
    fn emit_text_setting(&self, func: &mut Function, cell: CellId, body: &IrExpr) {
        if let IrExpr::TextConcat(segments) = body {
            // Collect all-literal text.
            let mut all_literal = true;
            let mut text = String::new();
            for seg in segments {
                match seg {
                    TextSegment::Literal(s) => text.push_str(s),
                    _ => { all_literal = false; break; }
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
    ) {
        // Emit nested if-else chain so only the FIRST matching arm executes.
        // Without this, wildcards would always overwrite earlier matches.
        self.emit_pattern_arms(func, source, arms, target, 0);
    }

    /// Recursively emit pattern match arms as nested if-else blocks.
    fn emit_pattern_arms(
        &self,
        func: &mut Function,
        source: CellId,
        arms: &[(IrPattern, IrExpr)],
        target: CellId,
        idx: usize,
    ) {
        if idx >= arms.len() {
            return;
        }

        let (pattern, body) = &arms[idx];
        let is_skip = matches!(body, IrExpr::Constant(IrValue::Skip));
        let has_more = idx + 1 < arms.len();

        match pattern {
            IrPattern::Tag(tag) => {
                let encoded = self.program.tag_table.iter()
                    .position(|t| t == tag)
                    .map(|i| (i + 1) as f64)
                    .unwrap_or(0.0);
                func.instruction(&Instruction::GlobalGet(source.0));
                func.instruction(&Instruction::F64Const(encoded));
                func.instruction(&Instruction::F64Eq);
                func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
                if !is_skip {
                    self.emit_arm_body(func, body, target);
                }
                if has_more {
                    func.instruction(&Instruction::Else);
                    self.emit_pattern_arms(func, source, arms, target, idx + 1);
                }
                func.instruction(&Instruction::End);
            }
            IrPattern::Number(n) => {
                func.instruction(&Instruction::GlobalGet(source.0));
                func.instruction(&Instruction::F64Const(*n));
                func.instruction(&Instruction::F64Eq);
                func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
                if !is_skip {
                    self.emit_arm_body(func, body, target);
                }
                if has_more {
                    func.instruction(&Instruction::Else);
                    self.emit_pattern_arms(func, source, arms, target, idx + 1);
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
                    self.emit_arm_body(func, body, target);
                }
                if has_more {
                    func.instruction(&Instruction::Else);
                    self.emit_pattern_arms(func, source, arms, target, idx + 1);
                }
                func.instruction(&Instruction::End);
            }
            IrPattern::Wildcard | IrPattern::Binding(_) => {
                // Wildcard matches everything — no remaining arms matter.
                if !is_skip {
                    self.emit_arm_body(func, body, target);
                }
            }
        }
    }

    /// Emit a WHEN/WHILE arm body: evaluate expression, set target cell, copy text if
    /// the body reads from another cell, and propagate downstream.
    fn emit_arm_body(&self, func: &mut Function, body: &IrExpr, target: CellId) {
        // Before evaluating the body, re-evaluate any block-local dependency chain.
        // If the body is CellRead(cell), walk up the node graph for that cell
        // and re-evaluate TextTrim/TextIsNotEmpty/Derived(CellRead) nodes.
        self.emit_reevaluate_chain(func, body);

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
        self.emit_downstream_updates(func, target);
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
        if let Some(node) = self.find_node_for_cell(cell) {
            match node {
                IrNode::Derived { expr: IrExpr::CellRead(source), .. } => {
                    // Re-evaluate upstream first.
                    self.emit_reevaluate_cell(func, *source);
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
                    self.emit_reevaluate_cell(func, *source);
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
                    self.emit_reevaluate_cell(func, *source);
                    func.instruction(&Instruction::I32Const(source.0 as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_TEXT_IS_NOT_EMPTY));
                    func.instruction(&Instruction::GlobalSet(cell.0));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                }
                IrNode::When { source, arms, .. } => {
                    self.emit_reevaluate_cell(func, *source);
                    // Re-evaluate the pattern match inline.
                    self.emit_pattern_match(func, *source, arms, cell);
                }
                IrNode::PipeThrough { source, .. } => {
                    self.emit_reevaluate_cell(func, *source);
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
    }

    /// Find the IrNode that defines a cell (for re-evaluation).
    fn find_node_for_cell(&self, cell: CellId) -> Option<&IrNode> {
        for node in &self.program.nodes {
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
                    if *c == cell { return Some(node); }
                }
                IrNode::Document { .. } | IrNode::Timer { .. } => {}
            }
        }
        None
    }

    /// Resolve the display text for a cell statically by following CellRead chains
    /// through Derived and HOLD nodes to find literal text (TextConcat with all literals,
    /// Constant(Text), or Constant(Tag)). Returns None if text can't be resolved statically.
    fn resolve_cell_text_statically(&self, cell: CellId) -> Option<String> {
        self.resolve_cell_text_statically_depth(cell, 0)
    }

    fn resolve_cell_text_statically_depth(&self, cell: CellId, depth: u32) -> Option<String> {
        if depth > 20 { return None; }
        let node = self.find_node_for_cell(cell)?;
        match node {
            IrNode::Hold { init, .. } => {
                self.resolve_expr_text_statically_depth(init, depth + 1)
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
        if depth > 20 { return None; }
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
            _ => None,
        }
    }

    /// For a namespace cell (object), find the first text-bearing field cell
    /// and resolve its text statically.
    fn resolve_namespace_text_statically(&self, cell: CellId) -> Option<String> {
        if let Some(fields) = self.program.cell_field_cells.get(&cell) {
            for (_name, field_cell) in fields.iter() {
                if let Some(text) = self.resolve_cell_text_statically(*field_cell) {
                    if !text.is_empty() {
                        return Some(text);
                    }
                }
            }
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
    fn find_text_source_for_namespace(&self, cell: CellId) -> Option<CellId> {
        let fields = self.program.cell_field_cells.get(&cell)?;
        for (_name, field_cell) in fields.iter() {
            if let Some(node) = self.find_node_for_cell(*field_cell) {
                match node {
                    IrNode::Hold { init, .. } => {
                        if let IrExpr::CellRead(source) = init {
                            return Some(*source);
                        }
                    }
                    IrNode::Derived { expr: IrExpr::CellRead(source), .. } => {
                        return Some(*source);
                    }
                    IrNode::Derived { expr: IrExpr::TextConcat(_), .. } => {
                        return Some(*field_cell);
                    }
                    _ => {}
                }
            }
        }
        None
    }

    /// After a list mutation (append/clear), update downstream ListCount and ListMap cells.
    fn emit_list_downstream_updates(&self, func: &mut Function, list_cell: CellId) {
        for node in &self.program.nodes {
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
                    func.instruction(&Instruction::If(wasm_encoder::BlockType::Result(ValType::F64)));
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
                // ListAppend/ListClear/ListRemove/ListRetain chain
                IrNode::ListAppend { cell, source, .. } if *source == list_cell => {
                    self.emit_list_downstream_updates(func, *cell);
                }
                IrNode::ListClear { cell, source, .. } if *source == list_cell => {
                    self.emit_list_downstream_updates(func, *cell);
                }
                IrNode::ListRemove { cell, source, .. } if *source == list_cell => {
                    self.emit_list_downstream_updates(func, *cell);
                }
                IrNode::ListRetain { cell, source, predicate, item_field_cells, .. }
                    if *source == list_cell =>
                {
                    if let (Some(pred), Some((l0, l1, l2))) = (predicate, *self.filter_locals.borrow()) {
                        if !item_field_cells.is_empty() {
                            // Per-item filtering: re-run filter loop when source list changes.
                            self.emit_retain_filter_loop(
                                func, *cell, *source, *pred,
                                item_field_cells, l0, l1, l2,
                            );
                        }
                    }
                    self.emit_list_downstream_updates(func, *cell);
                }
                _ => {}
            }
        }
    }

    /// After updating a cell, check for downstream nodes (e.g., MathSum) that need updating.
    fn emit_downstream_updates(&self, func: &mut Function, updated_cell: CellId) {
        for node in &self.program.nodes {
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
                IrNode::Derived { cell, expr: IrExpr::CellRead(source) } if *source == updated_cell => {
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
                    self.emit_expr(func, expr);
                    func.instruction(&Instruction::GlobalSet(cell.0));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    func.instruction(&Instruction::GlobalGet(cell.0));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    self.emit_downstream_updates(func, *cell);
                }
                // WHEN re-evaluates when source cell changes.
                IrNode::When { cell, source, arms } if *source == updated_cell => {
                    self.emit_pattern_match(func, *source, arms, *cell);
                }
                // WHILE re-evaluates when source OR any dep changes.
                IrNode::While { cell, source, deps, arms }
                    if *source == updated_cell || deps.contains(&updated_cell) =>
                {
                    self.emit_pattern_match(func, *source, arms, *cell);
                }
                // ListIsEmpty re-evaluates when source changes.
                IrNode::ListIsEmpty { cell, source } if *source == updated_cell => {
                    func.instruction(&Instruction::I32Const(source.0 as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_LIST_COUNT));
                    func.instruction(&Instruction::F64Const(0.0));
                    func.instruction(&Instruction::F64Eq);
                    func.instruction(&Instruction::If(wasm_encoder::BlockType::Result(ValType::F64)));
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
                IrNode::ListAppend { cell, source, item, .. } if *item == updated_cell => {
                    // Append text from item cell to the list.
                    func.instruction(&Instruction::I32Const(source.0 as i32));
                    func.instruction(&Instruction::I32Const(item.0 as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_LIST_APPEND_TEXT));
                    self.emit_list_downstream_updates(func, *cell);
                }
                // ListAppend triggered by watch_cell change (reactive dependency).
                IrNode::ListAppend { cell, source, watch_cell: Some(watch), .. }
                    if *watch == updated_cell =>
                {
                    // The watch cell changed — check for SKIP sentinel before appending.
                    // Read the watch cell value; if NaN (SKIP), don't append.
                    func.instruction(&Instruction::GlobalGet(watch.0));
                    func.instruction(&Instruction::GlobalGet(watch.0));
                    func.instruction(&Instruction::F64Ne); // NaN != NaN → true (i.e., is NaN)
                    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
                    // SKIP: do nothing
                    func.instruction(&Instruction::Else);
                    // Not SKIP: append text from watch cell (the reactive source) to the list.
                    func.instruction(&Instruction::I32Const(source.0 as i32));
                    func.instruction(&Instruction::I32Const(watch.0 as i32));
                    func.instruction(&Instruction::Call(IMPORT_HOST_LIST_APPEND_TEXT));
                    self.emit_list_downstream_updates(func, *cell);
                    func.instruction(&Instruction::End);
                }
                // ListRetain: when predicate cell changes, re-evaluate retain.
                IrNode::ListRetain { cell, source, predicate: Some(pred), item_field_cells, .. }
                    if *pred == updated_cell =>
                {
                    if let (true, Some((l0, l1, l2))) = (!item_field_cells.is_empty(), *self.filter_locals.borrow()) {
                        // Per-item filtering: run filter loop.
                        self.emit_retain_filter_loop(
                            func, *cell, *source, *pred,
                            item_field_cells, l0, l1, l2,
                        );
                        self.emit_list_downstream_updates(func, *cell);
                    } else if item_field_cells.is_empty() {
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
            IrExpr::UnaryNeg(inner) | IrExpr::Not(inner) => {
                Self::expr_references_cell(inner, cell)
            }
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
                let encoded = self.program.tag_table.iter()
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
                func.instruction(&Instruction::If(wasm_encoder::BlockType::Result(ValType::F64)));
                func.instruction(&Instruction::F64Const(0.0));
                func.instruction(&Instruction::Else);
                func.instruction(&Instruction::F64Const(1.0));
                func.instruction(&Instruction::End);
            }
            IrExpr::Compare { op, lhs, rhs } => {
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
            IrExpr::FunctionCall { func: _func_id, args: _ } => {
                func.instruction(&Instruction::F64Const(0.0));
            }
            IrExpr::ObjectConstruct(_) => {
                func.instruction(&Instruction::F64Const(0.0));
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
        }
    }

    // -----------------------------------------------------------------------
    // Per-item memory helpers
    // -----------------------------------------------------------------------

    /// Emit instructions to read a cell value (f64) onto the stack.
    /// If mem_ctx is Some and cell is in template range, reads from linear memory.
    /// Otherwise, reads from the WASM global.
    fn emit_cell_get(&self, func: &mut Function, cell: CellId, mem_ctx: Option<&MemoryContext>) {
        if let Some(ctx) = mem_ctx {
            if cell.0 >= ctx.cell_start && cell.0 < ctx.cell_end {
                self.emit_mem_addr(func, ctx, cell);
                func.instruction(&Instruction::F64Load(wasm_encoder::MemArg {
                    offset: 0,
                    align: 3, // 2^3 = 8 byte alignment
                    memory_index: 0,
                }));
                return;
            }
        }
        func.instruction(&Instruction::GlobalGet(cell.0));
    }

    /// Emit instructions to write a cell value. Value must already be on the stack.
    /// If mem_ctx is Some and cell is in template range, writes to linear memory.
    /// Otherwise, writes to the WASM global.
    ///
    /// NOTE: For memory writes, the address must be pushed BEFORE the value.
    /// This method expects the value is already on the stack and pushes addr underneath
    /// using a temp local. Callers should use `emit_cell_set_with_addr` for the
    /// push-addr-then-value pattern.
    fn emit_cell_set(&self, func: &mut Function, cell: CellId, mem_ctx: Option<&MemoryContext>) {
        if let Some(ctx) = mem_ctx {
            if cell.0 >= ctx.cell_start && cell.0 < ctx.cell_end {
                // Value is on stack. Store to global temp, push addr, get value back, store.
                let temp_global = self.program.cells.len() as u32; // temp global
                func.instruction(&Instruction::GlobalSet(temp_global));
                self.emit_mem_addr(func, ctx, cell);
                func.instruction(&Instruction::GlobalGet(temp_global));
                func.instruction(&Instruction::F64Store(wasm_encoder::MemArg {
                    offset: 0,
                    align: 3,
                    memory_index: 0,
                }));
                return;
            }
        }
        func.instruction(&Instruction::GlobalSet(cell.0));
    }

    /// Emit memory address computation for a per-item cell.
    /// Pushes i32 address onto the stack.
    fn emit_mem_addr(&self, func: &mut Function, ctx: &MemoryContext, cell: CellId) {
        let local_offset = (cell.0 - ctx.cell_start) * 8;
        func.instruction(&Instruction::LocalGet(ctx.item_idx_local));
        func.instruction(&Instruction::I32Const(ctx.stride as i32));
        func.instruction(&Instruction::I32Mul);
        func.instruction(&Instruction::I32Const((ctx.memory_base + local_offset) as i32));
        func.instruction(&Instruction::I32Add);
    }

    /// Find the first ListMap node and return its template ranges.
    fn find_list_map_info(&self) -> Option<(u32, u32, u32, u32)> {
        for node in &self.program.nodes {
            if let IrNode::ListMap { template_cell_range, template_event_range, .. } = node {
                return Some((
                    template_cell_range.0,
                    template_cell_range.1,
                    template_event_range.0,
                    template_event_range.1,
                ));
            }
        }
        None
    }

    /// Build a MemoryContext for the first ListMap in the program.
    fn build_memory_context(&self, item_idx_local: u32) -> Option<MemoryContext> {
        let (cell_start, cell_end, _, _) = self.find_list_map_info()?;
        let cell_count = cell_end - cell_start;
        if cell_count == 0 { return None; }
        Some(MemoryContext {
            item_idx_local,
            memory_base: 0,
            stride: cell_count * 8,
            cell_start,
            cell_end,
        })
    }

    // -----------------------------------------------------------------------
    // Per-item retain (filter loop)
    // -----------------------------------------------------------------------

    /// Check if any ListRetain has per-item filtering.
    fn has_per_item_retain(&self) -> bool {
        self.program.nodes.iter().any(|node| {
            matches!(node, IrNode::ListRetain { item_field_cells, .. } if !item_field_cells.is_empty())
        })
    }

    /// Find the ListMap's item_cell for looking up template field cells.
    fn find_list_map_item_cell(&self) -> Option<CellId> {
        for node in &self.program.nodes {
            if let IrNode::ListMap { item_cell, .. } = node {
                return Some(*item_cell);
            }
        }
        None
    }

    /// Emit a WASM filter loop that iterates source list items, evaluates the
    /// predicate per-item, and builds a filtered list via host_list_copy_item.
    ///
    /// `local_new_list`: f64 local for new list ID
    /// `local_count`: i32 local for item count
    /// `local_i`: i32 local for loop counter
    fn emit_retain_filter_loop(
        &self,
        func: &mut Function,
        retain_cell: CellId,
        source_cell: CellId,
        predicate: CellId,
        item_field_cells: &[(String, CellId)],
        local_new_list: u32,
        local_count: u32,
        local_i: u32,
    ) {
        let (cell_start, cell_end, _, _) = match self.find_list_map_info() {
            Some(info) => info,
            None => return,
        };
        let stride = (cell_end - cell_start) * 8;
        let list_map_item_cell = match self.find_list_map_item_cell() {
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
        func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));  // $continue

        // if (i >= count) break
        func.instruction(&Instruction::LocalGet(local_i));
        func.instruction(&Instruction::LocalGet(local_count));
        func.instruction(&Instruction::I32GeS);
        func.instruction(&Instruction::BrIf(1)); // br $break

        // Read per-item field values from WASM linear memory into field sub-cell globals.
        for (field_name, sub_cell) in item_field_cells {
            // Find the template field cell for this field name.
            let template_field_cell = self.program.cell_field_cells
                .get(&list_map_item_cell)
                .and_then(|fields| fields.get(field_name))
                .copied();
            if let Some(tfc) = template_field_cell {
                let field_offset = (tfc.0 - cell_start) * 8;
                // addr = i * stride + field_offset
                func.instruction(&Instruction::LocalGet(local_i));
                func.instruction(&Instruction::I32Const(stride as i32));
                func.instruction(&Instruction::I32Mul);
                func.instruction(&Instruction::I32Const(field_offset as i32));
                func.instruction(&Instruction::I32Add);
                // f64.load from linear memory
                func.instruction(&Instruction::F64Load(wasm_encoder::MemArg {
                    offset: 0,
                    align: 3, // 2^3 = 8 byte alignment
                    memory_index: 0,
                }));
                func.instruction(&Instruction::GlobalSet(sub_cell.0));
            }
        }

        // Re-evaluate any intermediate Derived cells that reference field sub-cells.
        // (e.g., Bool/not(item.completed) → Not(CellRead(completed_sub_cell)))
        let field_cell_ids: Vec<CellId> = item_field_cells.iter().map(|(_, c)| *c).collect();
        for node in &self.program.nodes {
            if let IrNode::Derived { cell, expr } = node {
                if field_cell_ids.iter().any(|fc| Self::expr_references_cell(expr, *fc)) {
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
                self.emit_pattern_arms_no_notify(func, *source, arms, predicate, 0);
            }
            Some(IrNode::When { source, arms, .. }) => {
                self.emit_pattern_arms_no_notify(func, *source, arms, predicate, 0);
            }
            _ => {
                // Predicate is directly a field sub-cell or simple derived —
                // already set by the field read or derived re-evaluation above.
            }
        }

        // If predicate is truthy, copy item to new list.
        func.instruction(&Instruction::GlobalGet(predicate.0));
        func.instruction(&Instruction::F64Const(0.0));
        func.instruction(&Instruction::F64Ne);
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

        // 4. Set retain output cell to new list
        func.instruction(&Instruction::LocalGet(local_new_list));
        func.instruction(&Instruction::GlobalSet(retain_cell.0));
        func.instruction(&Instruction::I32Const(retain_cell.0 as i32));
        func.instruction(&Instruction::GlobalGet(retain_cell.0));
        func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
    }

    /// Like `emit_pattern_arms` but without host notification or downstream propagation.
    /// Used inside filter loops where we only need the f64 predicate value.
    fn emit_pattern_arms_no_notify(
        &self,
        func: &mut Function,
        source: CellId,
        arms: &[(IrPattern, IrExpr)],
        target: CellId,
        idx: usize,
    ) {
        if idx >= arms.len() {
            return;
        }

        let (pattern, body) = &arms[idx];
        let is_skip = matches!(body, IrExpr::Constant(IrValue::Skip));
        let has_more = idx + 1 < arms.len();

        match pattern {
            IrPattern::Tag(tag) => {
                let encoded = self.program.tag_table.iter()
                    .position(|t| t == tag)
                    .map(|i| (i + 1) as f64)
                    .unwrap_or(0.0);
                func.instruction(&Instruction::GlobalGet(source.0));
                func.instruction(&Instruction::F64Const(encoded));
                func.instruction(&Instruction::F64Eq);
                func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
                if !is_skip {
                    self.emit_expr(func, body);
                    func.instruction(&Instruction::GlobalSet(target.0));
                }
                if has_more {
                    func.instruction(&Instruction::Else);
                    self.emit_pattern_arms_no_notify(func, source, arms, target, idx + 1);
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
                    func.instruction(&Instruction::GlobalSet(target.0));
                }
                if has_more {
                    func.instruction(&Instruction::Else);
                    self.emit_pattern_arms_no_notify(func, source, arms, target, idx + 1);
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
                    func.instruction(&Instruction::GlobalSet(target.0));
                }
                if has_more {
                    func.instruction(&Instruction::Else);
                    self.emit_pattern_arms_no_notify(func, source, arms, target, idx + 1);
                }
                func.instruction(&Instruction::End);
            }
            IrPattern::Wildcard | IrPattern::Binding(_) => {
                if !is_skip {
                    self.emit_expr(func, body);
                    func.instruction(&Instruction::GlobalSet(target.0));
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Per-item WASM function emitters
    // -----------------------------------------------------------------------

    /// Emit `init_item(item_idx: i32)`.
    /// Initializes all template cells in WASM memory for a new item.
    fn emit_init_item(&self) -> Function {
        let mut func = Function::new([]);
        let mem_ctx = self.build_memory_context(0); // local 0 = item_idx

        let mem_ctx = match mem_ctx {
            Some(ctx) => ctx,
            None => {
                func.instruction(&Instruction::End);
                return func;
            }
        };

        // Set item context so host routes updates to per-item Mutables.
        func.instruction(&Instruction::LocalGet(0)); // item_idx
        func.instruction(&Instruction::Call(IMPORT_HOST_SET_ITEM_CONTEXT));

        // Initialize each template-scoped node.
        for node in &self.program.nodes {
            match node {
                IrNode::Hold { cell, init, .. }
                    if cell.0 >= mem_ctx.cell_start && cell.0 < mem_ctx.cell_end =>
                {
                    // Evaluate init expr → store to memory.
                    self.emit_expr_ctx(&mut func, init, Some(&mem_ctx));
                    self.emit_cell_set(&mut func, *cell, Some(&mem_ctx));
                    // Notify host of per-item cell value.
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    self.emit_cell_get(&mut func, *cell, Some(&mem_ctx));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    // Copy text from init source cell to HOLD cell so per-item
                    // text (e.g., todo title) propagates through HOLD.
                    if let IrExpr::CellRead(src) = init {
                        func.instruction(&Instruction::I32Const(cell.0 as i32));
                        func.instruction(&Instruction::I32Const(src.0 as i32));
                        func.instruction(&Instruction::Call(IMPORT_HOST_COPY_TEXT));
                    }
                }
                IrNode::Derived { cell, expr }
                    if cell.0 >= mem_ctx.cell_start && cell.0 < mem_ctx.cell_end =>
                {
                    self.emit_expr_ctx(&mut func, expr, Some(&mem_ctx));
                    self.emit_cell_set(&mut func, *cell, Some(&mem_ctx));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    self.emit_cell_get(&mut func, *cell, Some(&mem_ctx));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                }
                IrNode::When { cell, source, arms }
                    if cell.0 >= mem_ctx.cell_start && cell.0 < mem_ctx.cell_end =>
                {
                    self.emit_pattern_match_ctx(&mut func, *source, arms, *cell, Some(&mem_ctx));
                }
                IrNode::While { cell, source, arms, .. }
                    if cell.0 >= mem_ctx.cell_start && cell.0 < mem_ctx.cell_end =>
                {
                    self.emit_pattern_match_ctx(&mut func, *source, arms, *cell, Some(&mem_ctx));
                }
                _ => {}
            }
        }

        // Clear item context.
        func.instruction(&Instruction::Call(IMPORT_HOST_CLEAR_ITEM_CONTEXT));
        func.instruction(&Instruction::End);
        func
    }

    /// Emit `on_item_event(item_idx: i32, event_id: i32)`.
    /// Handles per-item events using br_table dispatch.
    fn emit_on_item_event(&self) -> Function {
        // local 0 = item_idx, local 1 = event_id
        let mut func = Function::new([]);
        let mem_ctx = self.build_memory_context(0); // local 0 = item_idx

        let mem_ctx = match mem_ctx {
            Some(ctx) => ctx,
            None => {
                func.instruction(&Instruction::End);
                return func;
            }
        };

        let (_, _, event_start, event_end) = self.find_list_map_info().unwrap();

        // Collect ALL event IDs that trigger template-scoped nodes.
        // This includes both template-local events AND global events that trigger template HOLDs.
        let mut relevant_events: Vec<u32> = Vec::new();
        for node in &self.program.nodes {
            match node {
                IrNode::Then { trigger, cell, .. }
                    if cell.0 >= mem_ctx.cell_start && cell.0 < mem_ctx.cell_end =>
                {
                    if !relevant_events.contains(&trigger.0) {
                        relevant_events.push(trigger.0);
                    }
                }
                IrNode::Hold { trigger_bodies, cell, .. }
                    if cell.0 >= mem_ctx.cell_start && cell.0 < mem_ctx.cell_end =>
                {
                    for (t, _) in trigger_bodies {
                        if !relevant_events.contains(&t.0) {
                            relevant_events.push(t.0);
                        }
                    }
                }
                IrNode::Latest { target, arms }
                    if target.0 >= mem_ctx.cell_start && target.0 < mem_ctx.cell_end =>
                {
                    for arm in arms {
                        if let Some(t) = arm.trigger {
                            if !relevant_events.contains(&t.0) {
                                relevant_events.push(t.0);
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        if relevant_events.is_empty() {
            func.instruction(&Instruction::End);
            return func;
        }

        // Set item context.
        func.instruction(&Instruction::LocalGet(0)); // item_idx
        func.instruction(&Instruction::Call(IMPORT_HOST_SET_ITEM_CONTEXT));

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
            if relevant_events.contains(&(idx as u32)) {
                self.emit_item_event_handler(&mut func, event_id, &mem_ctx);
            }
            let exit_depth = (num_all_events - 1 - idx) as u32;
            func.instruction(&Instruction::Br(exit_depth));
        }

        func.instruction(&Instruction::End); // end $exit

        // Clear item context.
        func.instruction(&Instruction::Call(IMPORT_HOST_CLEAR_ITEM_CONTEXT));
        func.instruction(&Instruction::End);
        func
    }

    /// Emit handler for a per-item event.
    fn emit_item_event_handler(&self, func: &mut Function, event_id: EventId, mem_ctx: &MemoryContext) {
        for node in &self.program.nodes {
            match node {
                IrNode::Then { cell, trigger, body }
                    if *trigger == event_id
                        && cell.0 >= mem_ctx.cell_start && cell.0 < mem_ctx.cell_end =>
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
                IrNode::Hold { cell, trigger_bodies, .. }
                    if cell.0 >= mem_ctx.cell_start && cell.0 < mem_ctx.cell_end =>
                {
                    for (trigger, body) in trigger_bodies {
                        if *trigger == event_id {
                            self.emit_expr_ctx(func, body, Some(mem_ctx));
                            self.emit_cell_set(func, *cell, Some(mem_ctx));
                            func.instruction(&Instruction::I32Const(cell.0 as i32));
                            self.emit_cell_get(func, *cell, Some(mem_ctx));
                            func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                            self.emit_item_downstream_updates(func, *cell, mem_ctx);
                        }
                    }
                }
                IrNode::Latest { target, arms }
                    if target.0 >= mem_ctx.cell_start && target.0 < mem_ctx.cell_end =>
                {
                    for arm in arms {
                        if arm.trigger == Some(event_id) {
                            if self.is_text_body(&arm.body) {
                                self.emit_text_setting_ctx(func, *target, &arm.body, Some(mem_ctx));
                            } else {
                                self.emit_expr_ctx(func, &arm.body, Some(mem_ctx));
                                self.emit_cell_set(func, *target, Some(mem_ctx));
                            }
                            func.instruction(&Instruction::I32Const(target.0 as i32));
                            self.emit_cell_get(func, *target, Some(mem_ctx));
                            func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                            self.emit_item_downstream_updates(func, *target, mem_ctx);
                        }
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
        if let IrExpr::TextConcat(segments) = body {
            let mut all_literal = true;
            let mut text = String::new();
            for seg in segments {
                match seg {
                    TextSegment::Literal(s) => text.push_str(s),
                    _ => { all_literal = false; break; }
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
    ) {
        self.emit_pattern_arms_ctx(func, source, arms, target, 0, mem_ctx);
    }

    fn emit_pattern_arms_ctx(
        &self,
        func: &mut Function,
        source: CellId,
        arms: &[(IrPattern, IrExpr)],
        target: CellId,
        idx: usize,
        mem_ctx: Option<&MemoryContext>,
    ) {
        if idx >= arms.len() { return; }
        let (pattern, body) = &arms[idx];
        let is_skip = matches!(body, IrExpr::Constant(IrValue::Skip));
        let has_more = idx + 1 < arms.len();

        match pattern {
            IrPattern::Tag(tag) => {
                let encoded = self.program.tag_table.iter()
                    .position(|t| t == tag)
                    .map(|i| (i + 1) as f64)
                    .unwrap_or(0.0);
                self.emit_cell_get(func, source, mem_ctx);
                func.instruction(&Instruction::F64Const(encoded));
                func.instruction(&Instruction::F64Eq);
                func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
                if !is_skip {
                    self.emit_arm_body_ctx(func, body, target, mem_ctx);
                }
                if has_more {
                    func.instruction(&Instruction::Else);
                    self.emit_pattern_arms_ctx(func, source, arms, target, idx + 1, mem_ctx);
                }
                func.instruction(&Instruction::End);
            }
            IrPattern::Number(n) => {
                self.emit_cell_get(func, source, mem_ctx);
                func.instruction(&Instruction::F64Const(*n));
                func.instruction(&Instruction::F64Eq);
                func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
                if !is_skip {
                    self.emit_arm_body_ctx(func, body, target, mem_ctx);
                }
                if has_more {
                    func.instruction(&Instruction::Else);
                    self.emit_pattern_arms_ctx(func, source, arms, target, idx + 1, mem_ctx);
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
                    self.emit_arm_body_ctx(func, body, target, mem_ctx);
                }
                if has_more {
                    func.instruction(&Instruction::Else);
                    self.emit_pattern_arms_ctx(func, source, arms, target, idx + 1, mem_ctx);
                }
                func.instruction(&Instruction::End);
            }
            IrPattern::Wildcard | IrPattern::Binding(_) => {
                if !is_skip {
                    self.emit_arm_body_ctx(func, body, target, mem_ctx);
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
        if let Some(ctx) = mem_ctx {
            self.emit_item_downstream_updates(func, target, ctx);
        } else {
            self.emit_downstream_updates(func, target);
        }
    }

    /// Emit downstream updates for template-scoped cells only.
    /// Only walks nodes in the template range and uses MemoryContext for access.
    fn emit_item_downstream_updates(&self, func: &mut Function, updated_cell: CellId, mem_ctx: &MemoryContext) {
        for node in &self.program.nodes {
            match node {
                IrNode::PipeThrough { cell, source }
                    if *source == updated_cell
                        && cell.0 >= mem_ctx.cell_start && cell.0 < mem_ctx.cell_end =>
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
                IrNode::Derived { cell, expr: IrExpr::CellRead(source) }
                    if *source == updated_cell
                        && cell.0 >= mem_ctx.cell_start && cell.0 < mem_ctx.cell_end =>
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
                        && cell.0 >= mem_ctx.cell_start && cell.0 < mem_ctx.cell_end =>
                {
                    self.emit_expr_ctx(func, expr, Some(mem_ctx));
                    self.emit_cell_set(func, *cell, Some(mem_ctx));
                    func.instruction(&Instruction::I32Const(cell.0 as i32));
                    self.emit_cell_get(func, *cell, Some(mem_ctx));
                    func.instruction(&Instruction::Call(IMPORT_HOST_SET_CELL_F64));
                    self.emit_item_downstream_updates(func, *cell, mem_ctx);
                }
                IrNode::When { cell, source, arms }
                    if *source == updated_cell
                        && cell.0 >= mem_ctx.cell_start && cell.0 < mem_ctx.cell_end =>
                {
                    self.emit_pattern_match_ctx(func, *source, arms, *cell, Some(mem_ctx));
                }
                IrNode::While { cell, source, deps, arms }
                    if (*source == updated_cell || deps.contains(&updated_cell))
                        && cell.0 >= mem_ctx.cell_start && cell.0 < mem_ctx.cell_end =>
                {
                    self.emit_pattern_match_ctx(func, *source, arms, *cell, Some(mem_ctx));
                }
                IrNode::TextIsNotEmpty { cell, source }
                    if *source == updated_cell
                        && cell.0 >= mem_ctx.cell_start && cell.0 < mem_ctx.cell_end =>
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
                        && cell.0 >= mem_ctx.cell_start && cell.0 < mem_ctx.cell_end =>
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

    /// Emit `get_item_cell(item_idx: i32, cell_offset: i32) -> f64`.
    /// Reads a per-item cell value from linear memory.
    fn emit_get_item_cell(&self) -> Function {
        let mut func = Function::new([]);
        let mem_ctx = self.build_memory_context(0); // local 0 = item_idx

        match mem_ctx {
            Some(ctx) => {
                // addr = item_idx * stride + cell_offset * 8 + memory_base
                func.instruction(&Instruction::LocalGet(0)); // item_idx
                func.instruction(&Instruction::I32Const(ctx.stride as i32));
                func.instruction(&Instruction::I32Mul);
                func.instruction(&Instruction::LocalGet(1)); // cell_offset
                func.instruction(&Instruction::I32Const(8));
                func.instruction(&Instruction::I32Mul);
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::I32Const(ctx.memory_base as i32));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::F64Load(wasm_encoder::MemArg {
                    offset: 0,
                    align: 3,
                    memory_index: 0,
                }));
            }
            None => {
                func.instruction(&Instruction::F64Const(0.0));
            }
        }

        func.instruction(&Instruction::End);
        func
    }
}
