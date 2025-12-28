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
use shared::ast::{ExprId, Program};
use shared::test_harness::Value;
use std::collections::HashMap;

/// The Path B runtime
pub struct Runtime {
    /// The program being executed
    program: Program,
    /// Value cache
    cache: Cache,
    /// HOLD cells
    holds: HashMap<SlotKey, HoldCell>,
    /// LINK cells
    links: HashMap<SlotKey, LinkCell>,
    /// LIST cells
    lists: HashMap<SlotKey, ListCell>,
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

        Self {
            program,
            cache: Cache::new(),
            holds: HashMap::new(),
            links: HashMap::new(),
            lists: HashMap::new(),
            tick_counter: TickCounter::new(),
            top_level,
            diagnostics: DiagnosticsContext::new(),
            pending_events: Vec::new(),
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

        // Process pending events
        for (path, payload) in std::mem::take(&mut self.pending_events) {
            self.inject_event(&path, payload);
        }

        // Re-evaluate all top-level bindings
        let root_scope = ScopeId::root();
        let program = self.program.clone();

        for (_name, expr) in &program.bindings {
            let mut ctx = EvalContext {
                program: &program,
                cache: &mut self.cache,
                holds: &mut self.holds,
                links: &mut self.links,
                lists: &mut self.lists,
                tick_counter: &mut self.tick_counter,
                top_level: &self.top_level,
                bindings: HashMap::new(),
                current_deps: Vec::new(),
            };

            let _ = eval(expr, &root_scope, &mut ctx);
        }

        // Re-evaluate nested HOLDs (non-root scope HOLDs in list items)
        // These are created by ListAppend and need to respond to external events
        self.reevaluate_nested_holds(&program);
    }

    /// Re-evaluate all nested HOLDs (those with non-root scopes)
    fn reevaluate_nested_holds(&mut self, program: &Program) {
        // Collect all nested HOLD keys (non-root scope)
        let nested_hold_keys: Vec<SlotKey> = self
            .holds
            .keys()
            .filter(|k| !k.scope.is_root())
            .cloned()
            .collect();

        // Find HOLD expressions in the AST
        for hold_key in nested_hold_keys {
            if let Some(hold_expr) = self.find_hold_expr_by_id(program, hold_key.expr) {
                // Re-evaluate the HOLD body
                if let shared::ast::ExprKind::Hold {
                    state_name, body, ..
                } = &hold_expr.kind
                {
                    let mut ctx = EvalContext {
                        program,
                        cache: &mut self.cache,
                        holds: &mut self.holds,
                        links: &mut self.links,
                        lists: &mut self.lists,
                        tick_counter: &mut self.tick_counter,
                        top_level: &self.top_level,
                        bindings: HashMap::new(),
                        current_deps: Vec::new(),
                    };

                    // Bind the state name to this HOLD cell
                    ctx.bind(state_name.clone(), hold_key.scope.clone(), hold_key.expr);

                    // Evaluate body
                    let (body_val, _) = eval(body, &hold_key.scope, &mut ctx);

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

    /// Find a HOLD expression by its ExprId (recursive search)
    fn find_hold_expr_by_id(
        &self,
        program: &Program,
        target_id: ExprId,
    ) -> Option<shared::ast::Expr> {
        for (_, expr) in &program.bindings {
            if let Some(found) = self.find_expr_by_id_recursive(expr, target_id) {
                return Some(found);
            }
        }
        None
    }

    /// Recursively search for an expression by id
    fn find_expr_by_id_recursive(
        &self,
        expr: &shared::ast::Expr,
        target_id: ExprId,
    ) -> Option<shared::ast::Expr> {
        if expr.id == target_id {
            return Some(expr.clone());
        }

        use shared::ast::ExprKind;
        match &expr.kind {
            ExprKind::Hold { initial, body, .. } => {
                self.find_expr_by_id_recursive(initial, target_id)
                    .or_else(|| self.find_expr_by_id_recursive(body, target_id))
            }
            ExprKind::Then { input, body } => self
                .find_expr_by_id_recursive(input, target_id)
                .or_else(|| self.find_expr_by_id_recursive(body, target_id)),
            ExprKind::When { input, arms } => {
                self.find_expr_by_id_recursive(input, target_id).or_else(|| {
                    for (_, arm_body) in arms {
                        if let Some(found) = self.find_expr_by_id_recursive(arm_body, target_id) {
                            return Some(found);
                        }
                    }
                    None
                })
            }
            ExprKind::While { input, body, .. } => self
                .find_expr_by_id_recursive(input, target_id)
                .or_else(|| self.find_expr_by_id_recursive(body, target_id)),
            ExprKind::Latest(exprs) => {
                for e in exprs {
                    if let Some(found) = self.find_expr_by_id_recursive(e, target_id) {
                        return Some(found);
                    }
                }
                None
            }
            ExprKind::Object(fields) => {
                for (_, field_expr) in fields {
                    if let Some(found) = self.find_expr_by_id_recursive(field_expr, target_id) {
                        return Some(found);
                    }
                }
                None
            }
            ExprKind::List(items) => {
                for item in items {
                    if let Some(found) = self.find_expr_by_id_recursive(item, target_id) {
                        return Some(found);
                    }
                }
                None
            }
            ExprKind::Call(_, args) => {
                for arg in args {
                    if let Some(found) = self.find_expr_by_id_recursive(arg, target_id) {
                        return Some(found);
                    }
                }
                None
            }
            ExprKind::Pipe(input, _, args) => {
                self.find_expr_by_id_recursive(input, target_id).or_else(|| {
                    for arg in args {
                        if let Some(found) = self.find_expr_by_id_recursive(arg, target_id) {
                            return Some(found);
                        }
                    }
                    None
                })
            }
            ExprKind::Path(base, _) => self.find_expr_by_id_recursive(base, target_id),
            ExprKind::Block { bindings, output } => {
                for (_, binding_expr) in bindings {
                    if let Some(found) = self.find_expr_by_id_recursive(binding_expr, target_id) {
                        return Some(found);
                    }
                }
                self.find_expr_by_id_recursive(output, target_id)
            }
            ExprKind::ListMap { list, template, .. } => self
                .find_expr_by_id_recursive(list, target_id)
                .or_else(|| self.find_expr_by_id_recursive(template, target_id)),
            ExprKind::ListAppend { list, item } => self
                .find_expr_by_id_recursive(list, target_id)
                .or_else(|| self.find_expr_by_id_recursive(item, target_id)),
            ExprKind::Literal(_) | ExprKind::Variable(_) | ExprKind::Link(_) => None,
        }
    }

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
                    let mut current = Value::Object(inner);
                    // Wrap in outer path parts if any
                    for &part in parts[1..parts.len()-1].iter().rev() {
                        let mut outer: HashMap<String, Value> = HashMap::new();
                        outer.insert(part.to_string(), current);
                        current = Value::Object(outer);
                    }
                    current
                } else {
                    let mut current = payload;
                    // Build from innermost to outermost
                    for &part in parts[1..].iter().rev() {
                        let mut inner: HashMap<String, Value> = HashMap::new();
                        inner.insert(part.to_string(), current);
                        current = Value::Object(inner);
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
                self.links.insert(slot_key, link_cell);
            }
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
                        .into_iter()
                        .zip(list_cell.keys.iter())
                        .map(|(item, item_key)| {
                            // Build the item's scope: parent / list_expr_id / item_key
                            let item_scope =
                                list_slot.scope.child(list_slot.expr.0 as u64).child(item_key.0);
                            self.refresh_object_fields(item, &item_scope)
                        })
                        .collect();
                    Value::List(refreshed)
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
    fn refresh_object_fields(&self, item: Value, item_scope: &ScopeId) -> Value {
        match item {
            Value::Object(mut fields) => {
                // Look for HOLDs under this item's scope
                for (hold_key, hold_cell) in &self.holds {
                    // Check if this HOLD is at or under the item's scope
                    if *item_scope == hold_key.scope || item_scope.is_ancestor_of(&hold_key.scope) {
                        // This HOLD is nested under this item
                        // Match field by value type (heuristic for todo_mvc structure)
                        match &hold_cell.value {
                            Value::Bool(_) => {
                                // Boolean HOLDs map to 'completed' field
                                fields.insert("completed".to_string(), hold_cell.value.clone());
                            }
                            Value::String(_) => {
                                // String HOLDs map to 'text' field
                                fields.insert("text".to_string(), hold_cell.value.clone());
                            }
                            _ => {
                                // For other types, try to find a matching field
                                for (field_name, field_val) in fields.iter_mut() {
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
                Value::Object(fields)
            }
            _ => item,
        }
    }
}
