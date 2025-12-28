//! Evaluator for Path B engine.
//!
//! Evaluates expressions in scopes, using caching for efficiency.

use crate::cache::{Cache, CacheEntry};
use crate::cell::{HoldCell, LinkCell, ListCell};
use crate::scope::ScopeId;
use crate::slot::SlotKey;
use crate::tick::{TickCounter, TickSeq};
use crate::value::{is_skip, ops};
use rustc_hash::FxHashMap;
use shared::ast::{Expr, ExprId, ExprKind, Literal, Pattern, Program};
use shared::test_harness::Value;
use std::collections::HashMap;

/// Evaluation context
pub struct EvalContext<'a> {
    /// The program being evaluated
    pub program: &'a Program,
    /// Value cache
    pub cache: &'a mut Cache,
    /// HOLD cells - uses FxHashMap for faster lookups
    pub holds: &'a mut FxHashMap<SlotKey, HoldCell>,
    /// LINK cells - uses FxHashMap for faster lookups
    pub links: &'a mut FxHashMap<SlotKey, LinkCell>,
    /// LIST cells - uses FxHashMap for faster lookups
    pub lists: &'a mut FxHashMap<SlotKey, ListCell>,
    /// Tick counter
    pub tick_counter: &'a mut TickCounter,
    /// Top-level bindings (name -> ExprId)
    pub top_level: &'a HashMap<String, ExprId>,
    /// Current scope bindings (name -> (scope, expr))
    pub bindings: HashMap<String, (ScopeId, ExprId)>,
    /// Dependencies being tracked
    pub current_deps: Vec<SlotKey>,
}

impl<'a> EvalContext<'a> {
    /// Look up a variable
    pub fn lookup(&self, name: &str) -> Option<(ScopeId, ExprId)> {
        // Check local bindings first
        if let Some(binding) = self.bindings.get(name) {
            return Some(binding.clone());
        }
        // Check top-level
        if let Some(&expr_id) = self.top_level.get(name) {
            return Some((ScopeId::root(), expr_id));
        }
        None
    }

    /// Bind a variable in current scope
    pub fn bind(&mut self, name: String, scope: ScopeId, expr: ExprId) {
        self.bindings.insert(name, (scope, expr));
    }

    /// Track a dependency
    pub fn track_dep(&mut self, slot: SlotKey) {
        if !self.current_deps.contains(&slot) {
            self.current_deps.push(slot);
        }
    }

    /// Get current tick
    pub fn current_tick(&self) -> u64 {
        self.tick_counter.current_tick()
    }
}

/// Evaluate an expression in a scope
pub fn eval(expr: &Expr, scope: &ScopeId, ctx: &mut EvalContext) -> (Value, TickSeq) {
    let slot_key = SlotKey::new(scope.clone(), expr.id);
    let current_tick = ctx.current_tick();

    // Check cache - use tick-based invalidation for correctness
    // Note: is_valid() method exists for dependency-based invalidation
    // but requires more integration with event/LINK system to work correctly
    if let Some(entry) = ctx.cache.get(&slot_key) {
        if entry.is_current(current_tick) {
            let value = entry.value.clone();
            let ts = entry.last_changed;
            ctx.track_dep(slot_key);
            return (value, ts);
        }
    }

    // Evaluate
    let (value, ts) = eval_inner(expr, scope, ctx);

    // Update cache
    let deps = std::mem::take(&mut ctx.current_deps);
    let mut entry = CacheEntry::new(value.clone(), current_tick, ts);
    entry.deps = deps.into();
    ctx.cache.insert(slot_key.clone(), entry);

    ctx.track_dep(slot_key);
    (value, ts)
}

/// Inner evaluation (no caching)
fn eval_inner(expr: &Expr, scope: &ScopeId, ctx: &mut EvalContext) -> (Value, TickSeq) {
    let ts = ctx.tick_counter.next();

    match &expr.kind {
        ExprKind::Literal(lit) => {
            let value = literal_to_value(lit);
            (value, ts)
        }

        ExprKind::Variable(name) => {
            if let Some((var_scope, expr_id)) = ctx.lookup(name) {
                // Check if this is a HOLD state reference - return held value directly
                let slot_key = SlotKey::new(var_scope.clone(), expr_id);
                if let Some(cell) = ctx.holds.get(&slot_key) {
                    return (cell.value.clone(), ts);
                }

                // Find the expression in the program
                if let Some((_, var_expr)) = ctx.program.bindings.iter().find(|(_, e)| e.id == expr_id) {
                    return eval(var_expr, &var_scope, ctx);
                }
            }
            (Value::Skip, ts)
        }

        ExprKind::Path(base, field) => {
            let (base_val, base_ts) = eval(base, scope, ctx);
            let value = ops::get_field(&base_val, field);
            (value, base_ts)
        }

        ExprKind::Object(fields) => {
            let mut obj = HashMap::new();
            let mut max_ts = ts;
            for (name, field_expr) in fields {
                let (val, field_ts) = eval(field_expr, scope, ctx);
                obj.insert(name.clone(), val);
                if field_ts > max_ts {
                    max_ts = field_ts;
                }
            }
            (Value::Object(obj), max_ts)
        }

        ExprKind::List(items) => {
            let mut list = Vec::new();
            let mut max_ts = ts;
            for item_expr in items {
                let (val, item_ts) = eval(item_expr, scope, ctx);
                list.push(val);
                if item_ts > max_ts {
                    max_ts = item_ts;
                }
            }
            (Value::List(list), max_ts)
        }

        ExprKind::Call(name, args) => {
            let mut arg_values = Vec::new();
            let mut max_ts = ts;
            for arg in args {
                let (val, arg_ts) = eval(arg, scope, ctx);
                arg_values.push(val);
                if arg_ts > max_ts {
                    max_ts = arg_ts;
                }
            }
            let result = call_builtin(name, &arg_values);
            (result, max_ts)
        }

        ExprKind::Pipe(input, method, args) => {
            let (input_val, input_ts) = eval(input, scope, ctx);
            let mut arg_values = vec![input_val];
            let mut max_ts = input_ts;
            for arg in args {
                let (val, arg_ts) = eval(arg, scope, ctx);
                arg_values.push(val);
                if arg_ts > max_ts {
                    max_ts = arg_ts;
                }
            }
            let result = call_builtin(method, &arg_values);
            (result, max_ts)
        }

        ExprKind::Latest(exprs) => {
            let mut result = Value::Skip;
            let mut max_ts = ts;
            for sub_expr in exprs {
                let (val, sub_ts) = eval(sub_expr, scope, ctx);
                if !is_skip(&val) {
                    result = val;
                    max_ts = sub_ts;
                }
            }
            (result, max_ts)
        }

        ExprKind::Hold { initial, state_name, body } => {
            let slot_key = SlotKey::new(scope.clone(), expr.id);

            // Get or create hold cell
            let current_value = if let Some(cell) = ctx.holds.get(&slot_key) {
                cell.value.clone()
            } else {
                // Initialize the cell
                let (init_val, _) = eval(initial, scope, ctx);
                ctx.holds.insert(slot_key.clone(), HoldCell::new(init_val.clone()));
                init_val
            };

            // Bind state name
            ctx.bind(state_name.clone(), scope.clone(), expr.id);

            // Evaluate body
            let (body_val, body_ts) = eval(body, scope, ctx);

            // Update state if body produced a value
            if !is_skip(&body_val) {
                if let Some(cell) = ctx.holds.get_mut(&slot_key) {
                    cell.value = body_val.clone();
                }
                (body_val, body_ts)
            } else {
                (current_value, ts)
            }
        }

        ExprKind::Then { input, body } => {
            let (input_val, _) = eval(input, scope, ctx);
            if is_skip(&input_val) {
                (Value::Skip, ts)
            } else {
                eval(body, scope, ctx)
            }
        }

        ExprKind::When { input, arms } => {
            let (input_val, _) = eval(input, scope, ctx);
            if is_skip(&input_val) {
                return (Value::Skip, ts);
            }

            for (pattern, body) in arms {
                if pattern_matches(pattern, &input_val, &mut ctx.bindings, scope) {
                    return eval(body, scope, ctx);
                }
            }
            (Value::Skip, ts)
        }

        ExprKind::While { input, pattern, body } => {
            let (input_val, _) = eval(input, scope, ctx);
            if is_skip(&input_val) {
                return (Value::Skip, ts);
            }

            if pattern_matches(pattern, &input_val, &mut ctx.bindings, scope) {
                eval(body, scope, ctx)
            } else {
                (Value::Skip, ts)
            }
        }

        ExprKind::Link(_alias) => {
            let slot_key = SlotKey::new(scope.clone(), expr.id);

            // Get or create link cell
            let cell = ctx.links.entry(slot_key.clone()).or_insert_with(LinkCell::new);

            // Return pending event if any (peek, don't consume - multiple readers may need it)
            if let Some(event) = cell.peek_event() {
                (event.clone(), ctx.tick_counter.next())
            } else {
                (Value::Skip, ts)
            }
        }

        ExprKind::Block { bindings, output } => {
            // Evaluate bindings
            for (name, binding_expr) in bindings {
                let (val, _) = eval(binding_expr, scope, ctx);
                ctx.bind(name.clone(), scope.clone(), binding_expr.id);
                // Store the value in cache
                let binding_key = SlotKey::new(scope.clone(), binding_expr.id);
                ctx.cache.insert(binding_key, CacheEntry::new(val, ctx.current_tick(), ts));
            }

            // Evaluate output
            eval(output, scope, ctx)
        }

        ExprKind::ListMap { list, item_name, template } => {
            let _slot_key = SlotKey::new(scope.clone(), expr.id);
            let (list_val, _) = eval(list, scope, ctx);

            match list_val {
                Value::List(items) => {
                    let mut results = Vec::new();
                    for (idx, item) in items.iter().enumerate() {
                        // Create child scope for this item
                        let item_scope = scope.child(idx as u64);

                        // Bind item name
                        ctx.bind(item_name.clone(), item_scope.clone(), template.id);

                        // Store item value
                        let item_key = SlotKey::new(item_scope.clone(), template.id);
                        ctx.cache.insert(item_key, CacheEntry::new(item.clone(), ctx.current_tick(), ts));

                        // Evaluate template
                        let (result, _) = eval(template, &item_scope, ctx);
                        results.push(result);
                    }
                    (Value::List(results), ts)
                }
                _ => (Value::Skip, ts),
            }
        }

        ExprKind::ListAppend { list, item } => {
            let (list_val, _) = eval(list, scope, ctx);

            // Get or create a ListCell to track item allocations
            let list_slot = SlotKey::new(scope.clone(), expr.id);
            let item_key = {
                let list_cell = ctx.lists.entry(list_slot).or_insert_with(ListCell::new);
                list_cell.append()
            };

            // Create a child scope for this item
            // The scope path is: parent_scope -> list_expr_id -> item_key
            // This ensures each item's nested HOLDs get unique cells
            let item_scope = scope.child(expr.id.0 as u64).child(item_key.0);

            // Evaluate the item template IN the item's scope
            let (item_val, _) = eval(item, &item_scope, ctx);

            // Append to the list value
            let result = ops::list_append(&list_val, item_val);
            (result, ts)
        }
    }
}

/// Convert a literal to a value
fn literal_to_value(lit: &Literal) -> Value {
    match lit {
        Literal::Int(v) => Value::Int(*v),
        Literal::Float(v) => Value::Float(*v),
        Literal::String(v) => Value::String(v.clone()),
        Literal::Bool(v) => Value::Bool(*v),
        Literal::Unit => Value::Unit,
    }
}

/// Check if a pattern matches a value
fn pattern_matches(
    pattern: &Pattern,
    value: &Value,
    bindings: &mut HashMap<String, (ScopeId, ExprId)>,
    _scope: &ScopeId,
) -> bool {
    match pattern {
        Pattern::Wildcard => true,
        Pattern::Bind(_name) => {
            // Bind patterns match anything
            true
        }
        Pattern::Literal(lit) => {
            let pattern_value = literal_to_value(lit);
            &pattern_value == value
        }
        Pattern::Object(fields) => {
            if let Value::Object(obj) = value {
                fields.iter().all(|(name, sub_pattern)| {
                    if let Some(field_value) = obj.get(name) {
                        pattern_matches(sub_pattern, field_value, bindings, _scope)
                    } else {
                        false
                    }
                })
            } else {
                false
            }
        }
    }
}

/// Call a built-in function
fn call_builtin(name: &str, args: &[Value]) -> Value {
    match name {
        "add" => {
            if args.len() >= 2 {
                ops::add(&args[0], &args[1])
            } else {
                Value::Skip
            }
        }
        "Bool/not" => {
            if !args.is_empty() {
                ops::bool_not(&args[0])
            } else {
                Value::Skip
            }
        }
        "List/len" => {
            if !args.is_empty() {
                ops::list_len(&args[0])
            } else {
                Value::Skip
            }
        }
        "List/append" => {
            if args.len() >= 2 {
                ops::list_append(&args[0], args[1].clone())
            } else {
                Value::Skip
            }
        }
        "List/every" => {
            if !args.is_empty() {
                ops::list_every(&args[0], |v| matches!(v, Value::Bool(true)))
            } else {
                Value::Skip
            }
        }
        "Math/sum" => {
            if !args.is_empty() {
                match &args[0] {
                    Value::List(items) => {
                        let sum: i64 = items.iter().filter_map(|v| v.as_int()).sum();
                        Value::Int(sum)
                    }
                    _ => Value::Skip,
                }
            } else {
                Value::Skip
            }
        }
        _ => Value::Skip,
    }
}
