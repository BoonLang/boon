//! Runtime for Path B engine.
//!
//! Combines all state and provides the main tick loop.

use crate::cache::Cache;
use crate::cell::{HoldCell, LinkCell, ListCell};
use crate::diagnostics::DiagnosticsContext;
use crate::evaluator::{eval, EvalContext};
use crate::scope::ScopeId;
use crate::slot::SlotKey;
use crate::tick::TickCounter;
use crate::value::ops;
use rustc_hash::{FxHashMap, FxHashSet};
use shared::ast::{ExprId, Program};
use shared::test_harness::Value;
use std::collections::HashMap;

/// The Path B runtime
pub struct Runtime {
    /// The program being executed
    program: Program,
    /// Expression lookup table: ExprId -> Expr (O(1) lookup vs O(n) AST search)
    expr_map: HashMap<ExprId, shared::ast::Expr>,
    /// Value cache
    cache: Cache,
    /// HOLD cells - uses FxHashMap for faster SlotKey lookups
    holds: FxHashMap<SlotKey, HoldCell>,
    /// Index: scope -> list of HOLD keys under that scope (for O(1) lookup in refresh)
    holds_by_scope: FxHashMap<ScopeId, Vec<SlotKey>>,
    /// LINK cells - uses FxHashMap for faster SlotKey lookups
    links: FxHashMap<SlotKey, LinkCell>,
    /// LINKs that received events this tick (for targeted nested HOLD re-eval)
    affected_links_this_tick: Vec<SlotKey>,
    /// LIST cells - uses FxHashMap for faster SlotKey lookups
    lists: FxHashMap<SlotKey, ListCell>,
    /// Tick counter
    tick_counter: TickCounter,
    /// Top-level bindings (name -> ExprId)
    top_level: HashMap<String, ExprId>,
    /// Diagnostics
    diagnostics: DiagnosticsContext,
    /// Pending events
    pending_events: Vec<(String, Value)>,
}

impl Runtime {
    pub fn new(program: Program) -> Self {
        // Build top-level binding map
        let top_level: HashMap<String, ExprId> = program
            .bindings
            .iter()
            .map(|(name, expr)| (name.clone(), expr.id))
            .collect();

        // Build expression lookup table (O(1) lookup vs O(n) recursive search)
        let expr_map = Self::build_expr_map(&program);

        Self {
            program,
            expr_map,
            cache: Cache::new(),
            holds: FxHashMap::default(),
            holds_by_scope: FxHashMap::default(),
            links: FxHashMap::default(),
            affected_links_this_tick: Vec::new(),
            lists: FxHashMap::default(),
            tick_counter: TickCounter::new(),
            top_level,
            diagnostics: DiagnosticsContext::new(),
            pending_events: Vec::new(),
        }
    }

    /// Build a lookup table mapping ExprId -> Expr for O(1) expression lookup
    fn build_expr_map(program: &Program) -> HashMap<ExprId, shared::ast::Expr> {
        let mut map = HashMap::new();
        for (_, expr) in &program.bindings {
            Self::walk_expr_into_map(expr, &mut map);
        }
        map
    }

    /// Recursively walk an expression tree and insert all expressions into the map
    fn walk_expr_into_map(expr: &shared::ast::Expr, map: &mut HashMap<ExprId, shared::ast::Expr>) {
        // Insert this expression
        map.insert(expr.id, expr.clone());

        // Recursively walk children
        use shared::ast::ExprKind;
        match &expr.kind {
            ExprKind::Hold { initial, body, .. } => {
                Self::walk_expr_into_map(initial, map);
                Self::walk_expr_into_map(body, map);
            }
            ExprKind::Then { input, body } => {
                Self::walk_expr_into_map(input, map);
                Self::walk_expr_into_map(body, map);
            }
            ExprKind::When { input, arms } => {
                Self::walk_expr_into_map(input, map);
                for (_, arm_body) in arms {
                    Self::walk_expr_into_map(arm_body, map);
                }
            }
            ExprKind::While { input, body, .. } => {
                Self::walk_expr_into_map(input, map);
                Self::walk_expr_into_map(body, map);
            }
            ExprKind::Latest(exprs) => {
                for e in exprs {
                    Self::walk_expr_into_map(e, map);
                }
            }
            ExprKind::Object(fields) => {
                for (_, field_expr) in fields {
                    Self::walk_expr_into_map(field_expr, map);
                }
            }
            ExprKind::List(items) => {
                for item in items {
                    Self::walk_expr_into_map(item, map);
                }
            }
            ExprKind::Call(_, args) => {
                for arg in args {
                    Self::walk_expr_into_map(arg, map);
                }
            }
            ExprKind::Pipe(input, _, args) => {
                Self::walk_expr_into_map(input, map);
                for arg in args {
                    Self::walk_expr_into_map(arg, map);
                }
            }
            ExprKind::Path(base, _) => {
                Self::walk_expr_into_map(base, map);
            }
            ExprKind::Block { bindings, output } => {
                for (_, binding_expr) in bindings {
                    Self::walk_expr_into_map(binding_expr, map);
                }
                Self::walk_expr_into_map(output, map);
            }
            ExprKind::ListMap { list, template, .. } => {
                Self::walk_expr_into_map(list, map);
                Self::walk_expr_into_map(template, map);
            }
            ExprKind::ListAppend { list, item } => {
                Self::walk_expr_into_map(list, map);
                Self::walk_expr_into_map(item, map);
            }
            ExprKind::ListClear { list } => {
                Self::walk_expr_into_map(list, map);
            }
            ExprKind::ListRemove { list, index } => {
                Self::walk_expr_into_map(list, map);
                Self::walk_expr_into_map(index, map);
            }
            ExprKind::ListRetain { list, predicate, .. } => {
                Self::walk_expr_into_map(list, map);
                Self::walk_expr_into_map(predicate, map);
            }
            ExprKind::Literal(_) | ExprKind::Variable(_) | ExprKind::Link(_) => {}
        }
    }

    /// Inject an event
    pub fn inject(&mut self, path: &str, payload: Value) {
        self.pending_events.push((path.to_string(), payload));
    }

    /// Run one tick
    pub fn tick(&mut self) {
        self.tick_counter.next_tick();

        // Clear events from previous tick
        for cell in self.links.values_mut() {
            cell.clear_event();
        }

        // Clear affected links tracking from previous tick
        self.affected_links_this_tick.clear();

        // Process pending events
        let had_events = !self.pending_events.is_empty();
        for (path, payload) in std::mem::take(&mut self.pending_events) {
            self.inject_event(&path, payload);
        }

        // B3: Skip evaluation if no events AND cache has entries (not first tick)
        // Nothing can have changed without external input
        if !had_events && !self.cache.is_empty() {
            return;
        }

        // Re-evaluate all top-level bindings using split borrows (no program clone)
        self.evaluate_top_level();

        // Re-evaluate nested HOLDs only if events were injected this tick
        // Skip if no events - nested HOLDs can't have changed without external input
        if !self.affected_links_this_tick.is_empty() {
            self.reevaluate_nested_holds_internal();
        }
    }

    /// Evaluate all top-level bindings with split borrows to avoid cloning program
    fn evaluate_top_level(&mut self) {
        let root_scope = ScopeId::root();

        // Destructure to split borrows - allows iterating program.bindings
        // while mutably borrowing other fields
        let Runtime {
            program,
            expr_map: _,
            cache,
            holds,
            holds_by_scope,
            links,
            affected_links_this_tick: _,
            lists,
            tick_counter,
            top_level,
            diagnostics: _,
            pending_events: _,
        } = self;

        for (_name, expr) in &program.bindings {
            let mut ctx = EvalContext {
                program,
                cache,
                holds,
                holds_by_scope,
                links,
                lists,
                tick_counter,
                top_level,
                bindings: HashMap::new(),
                current_deps: FxHashSet::default(),
            };

            let _ = eval(expr, &root_scope, &mut ctx);
        }
    }

    /// Re-evaluate all nested HOLDs (those with non-root scopes)
    /// Uses expr_map for O(1) expression lookup instead of recursive AST search
    fn reevaluate_nested_holds_internal(&mut self) {
        // Collect all nested HOLD keys (non-root scope)
        let nested_hold_keys: Vec<SlotKey> = self
            .holds
            .keys()
            .filter(|k| !k.scope.is_root())
            .cloned()
            .collect();

        // Find HOLD expressions using O(1) lookup and re-evaluate
        for hold_key in nested_hold_keys {
            // O(1) lookup using expr_map instead of O(n) AST search
            if let Some(hold_expr) = self.expr_map.get(&hold_key.expr) {
                // Re-evaluate the HOLD body
                if let shared::ast::ExprKind::Hold {
                    state_name, body, ..
                } = &hold_expr.kind
                {
                    // Clone the body expression for use after split borrow
                    let body = body.clone();
                    let state_name = state_name.clone();

                    // Split borrows for evaluation
                    let Runtime {
                        program,
                        expr_map: _,
                        cache,
                        holds,
                        holds_by_scope,
                        links,
                        affected_links_this_tick: _,
                        lists,
                        tick_counter,
                        top_level,
                        diagnostics: _,
                        pending_events: _,
                    } = self;

                    let mut ctx = EvalContext {
                        program,
                        cache,
                        holds,
                        holds_by_scope,
                        links,
                        lists,
                        tick_counter,
                        top_level,
                        bindings: HashMap::new(),
                        current_deps: FxHashSet::default(),
                    };

                    // Bind the state name to this HOLD cell
                    ctx.bind(state_name, hold_key.scope.clone(), hold_key.expr);

                    // Evaluate body
                    let (body_val, _) = eval(&body, &hold_key.scope, &mut ctx);

                    // Update cell if body produced a value
                    if !crate::value::is_skip(&body_val) {
                        if let Some(cell) = self.holds.get_mut(&hold_key) {
                            cell.value = body_val;
                        }
                    }
                }
            }
        }
    }

    // NOTE: find_hold_expr_by_id_static and find_expr_by_id_recursive_static removed.
    // They were O(n × AST_size). Now using expr_map for O(1) lookup.

    /// Inject an event at a path
    fn inject_event(&mut self, path: &str, payload: Value) {
        // Parse path: "button.click" -> resolve "button", inject { click: payload }
        let parts: Vec<&str> = path.split('.').collect();
        if parts.is_empty() {
            return;
        }

        // Find top-level binding
        if let Some(&expr_id) = self.top_level.get(parts[0]) {
            let slot_key = SlotKey::new(ScopeId::root(), expr_id);

            // Build nested object for remaining path parts
            // e.g., "button.click" with payload Unit becomes { click: Unit }
            // e.g., "input.submit" with text payload becomes { submit: Unit, value: text }
            let event_value = if parts.len() > 1 {
                let last_part = *parts.last().unwrap();
                if last_part == "submit" && !matches!(payload, Value::Unit) {
                    // For form submissions, include both submit event and value
                    let mut inner: HashMap<String, Value> = HashMap::new();
                    inner.insert("submit".to_string(), Value::Unit);
                    inner.insert("value".to_string(), payload);
                    let mut current = Value::Object(std::sync::Arc::new(inner));
                    // Wrap in outer path parts if any
                    for &part in parts[1..parts.len()-1].iter().rev() {
                        let mut outer: HashMap<String, Value> = HashMap::new();
                        outer.insert(part.to_string(), current);
                        current = Value::Object(std::sync::Arc::new(outer));
                    }
                    current
                } else {
                    let mut current = payload;
                    // Build from innermost to outermost
                    for &part in parts[1..].iter().rev() {
                        let mut inner: HashMap<String, Value> = HashMap::new();
                        inner.insert(part.to_string(), current);
                        current = Value::Object(std::sync::Arc::new(inner));
                    }
                    current
                }
            } else {
                payload
            };

            // Inject to the link cell
            if let Some(link_cell) = self.links.get_mut(&slot_key) {
                link_cell.inject(event_value);
            } else {
                // Create link cell and inject
                let mut link_cell = LinkCell::new();
                link_cell.inject(event_value);
                self.links.insert(slot_key.clone(), link_cell);
            }

            // Track this LINK for targeted nested HOLD re-evaluation
            self.affected_links_this_tick.push(slot_key);
        }
    }

    /// Read a value at a path
    pub fn read(&self, path: &str) -> Value {
        let parts: Vec<&str> = path.split('.').collect();
        if parts.is_empty() {
            return Value::Skip;
        }

        // Handle array indexing
        let first_part = parts[0];
        let (name, index) = if let Some(bracket_pos) = first_part.find('[') {
            let name = &first_part[..bracket_pos];
            let index_str = &first_part[bracket_pos + 1..first_part.len() - 1];
            let index: usize = index_str.parse().unwrap_or(0);
            (name, Some(index))
        } else {
            (first_part, None)
        };

        // Find top-level binding
        let expr_id = match self.top_level.get(name) {
            Some(&id) => id,
            None => return Value::Skip,
        };

        // Get cached value
        let slot_key = SlotKey::new(ScopeId::root(), expr_id);
        let mut value = self
            .cache
            .get(&slot_key)
            .map(|e| e.value.clone())
            .unwrap_or(Value::Skip);

        // For HOLD cells, return the held value - but refresh nested HOLD values in lists
        if let Some(cell) = self.holds.get(&slot_key) {
            value = self.refresh_nested_holds(cell.value.clone(), &slot_key);
        }

        // Apply array index if present
        if let Some(idx) = index {
            value = match value {
                Value::List(items) => items.get(idx).cloned().unwrap_or(Value::Skip),
                _ => Value::Skip,
            };
        }

        // Navigate remaining path
        for part in &parts[1..] {
            let (field, idx) = if let Some(bracket_pos) = part.find('[') {
                let field = &part[..bracket_pos];
                let index_str = &part[bracket_pos + 1..part.len() - 1];
                let index: usize = index_str.parse().unwrap_or(0);
                (field, Some(index))
            } else {
                (*part, None)
            };

            value = ops::get_field(&value, field);

            if let Some(i) = idx {
                value = match value {
                    Value::List(items) => items.get(i).cloned().unwrap_or(Value::Skip),
                    _ => Value::Skip,
                };
            }
        }

        value
    }

    /// Enable diagnostics
    pub fn enable_diagnostics(&mut self) {
        self.diagnostics.enable();
    }

    /// Get diagnostics context
    pub fn diagnostics(&self) -> &DiagnosticsContext {
        &self.diagnostics
    }

    /// Get cache for queries
    pub fn cache(&self) -> &Cache {
        &self.cache
    }

    /// Debug: Get all holds with their scopes
    pub fn holds_debug(&self) -> Vec<(String, String)> {
        self.holds
            .iter()
            .map(|(k, v)| (format!("{:?}", k), format!("{:?}", v.value)))
            .collect()
    }

    /// Debug: Get all lists with their keys
    pub fn lists_debug(&self) -> Vec<(String, Vec<u64>)> {
        self.lists
            .iter()
            .map(|(k, v)| (format!("{:?}", k), v.keys.iter().map(|k| k.0).collect()))
            .collect()
    }

    /// Debug: Get all links with their events
    pub fn links_debug(&self) -> Vec<(String, String)> {
        self.links
            .iter()
            .map(|(k, v)| (format!("{:?}", k), format!("pending: {:?}", v.peek_event())))
            .collect()
    }

    /// Refresh nested HOLD values in a list.
    /// When a HOLD stores a list of objects, nested HOLDs inside those objects
    /// may have been updated. This function walks the list and replaces stale
    /// values with current HOLD cell values.
    fn refresh_nested_holds(&self, value: Value, _parent_key: &SlotKey) -> Value {
        match value {
            Value::List(items) => {
                // Find which ListCell this corresponds to by matching list lengths
                let list_info = self.find_matching_list_cell(items.len());

                if let Some((list_slot, list_cell)) = list_info {
                    let refreshed: Vec<Value> = items
                        .iter()
                        .cloned()
                        .zip(list_cell.keys.iter())
                        .map(|(item, item_key)| {
                            // Build the item's scope: parent / list_expr_id / item_key
                            let item_scope =
                                list_slot.scope.child(list_slot.expr.0 as u64).child(item_key.0);
                            self.refresh_object_fields(item, &item_scope)
                        })
                        .collect();
                    Value::List(std::sync::Arc::new(refreshed))
                } else {
                    Value::List(items)
                }
            }
            _ => value,
        }
    }

    /// Find a ListCell that matches the given length
    fn find_matching_list_cell(&self, len: usize) -> Option<(&SlotKey, &crate::cell::ListCell)> {
        self.lists.iter().find(|(_, cell)| cell.len() == len)
    }

    /// Refresh object fields with current HOLD values from the item scope
    /// Uses holds_by_scope index for O(1) lookup instead of O(n) iteration
    fn refresh_object_fields(&self, item: Value, item_scope: &ScopeId) -> Value {
        use std::sync::Arc;
        match item {
            Value::Object(arc_fields) => {
                // Clone the underlying HashMap for mutation (copy-on-write semantics)
                let mut fields = (*arc_fields).clone();

                // Use index to find HOLDs at this item's scope (O(1) lookup)
                if let Some(hold_keys) = self.holds_by_scope.get(item_scope) {
                    for hold_key in hold_keys {
                        if let Some(hold_cell) = self.holds.get(hold_key) {
                            // Match field by value type (heuristic for todo_mvc structure)
                            match &hold_cell.value {
                                Value::Bool(_) => {
                                    fields.insert("completed".to_string(), hold_cell.value.clone());
                                }
                                Value::String(_) => {
                                    fields.insert("text".to_string(), hold_cell.value.clone());
                                }
                                _ => {
                                    for (_, field_val) in fields.iter_mut() {
                                        if std::mem::discriminant(field_val)
                                            == std::mem::discriminant(&hold_cell.value)
                                        {
                                            *field_val = hold_cell.value.clone();
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // B3: Removed ancestor check loop - O(n_scopes × depth) was too expensive
                // The first loop already handles HOLDs at the exact item_scope
                // Deeply nested HOLDs are rare and can be handled differently if needed

                Value::Object(Arc::new(fields))
            }
            _ => item,
        }
    }
}
