//! Evaluator for Path B engine.
//!
//! Evaluates expressions in scopes, using caching for efficiency.

use crate::cache::{Cache, CacheEntry};
use crate::cell::{HoldCell, LinkCell, ListCell};
use crate::scope::ScopeId;
use crate::slot::SlotKey;
use crate::tick::{TickCounter, TickSeq};
use crate::value::{is_skip, ops};
use rustc_hash::{FxHashMap, FxHashSet};
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
    /// Index: scope -> list of HOLD keys under that scope (for O(1) lookup in refresh)
    pub holds_by_scope: &'a mut FxHashMap<ScopeId, Vec<SlotKey>>,
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
    /// Dependencies being tracked (FxHashSet for O(1) contains check)
    pub current_deps: FxHashSet<SlotKey>,
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

    /// Track a dependency (O(1) with FxHashSet)
    pub fn track_dep(&mut self, slot: SlotKey) {
        self.current_deps.insert(slot);
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

    // B2: Check cache with pure expression optimization
    // Pure expressions don't depend on events/HOLD, so they're valid across ticks
    if let Some(entry) = ctx.cache.get(&slot_key) {
        // Pure expressions: return cached value regardless of tick
        if entry.is_pure {
            let value = entry.value.clone();
            let ts = entry.last_changed;
            ctx.track_dep(slot_key);
            return (value, ts);
        }
        // Impure expressions: only valid for current tick
        if entry.is_current(current_tick) {
            let value = entry.value.clone();
            let ts = entry.last_changed;
            ctx.track_dep(slot_key);
            return (value, ts);
        }
    }

    // Evaluate and track purity
    let (value, ts, is_pure) = eval_inner_with_purity(expr, scope, ctx);

    // Update cache
    let deps = std::mem::take(&mut ctx.current_deps);
    let mut entry = if is_pure {
        CacheEntry::new_pure(value.clone(), current_tick, ts)
    } else {
        CacheEntry::new(value.clone(), current_tick, ts)
    };
    entry.deps = deps.into_iter().collect();
    ctx.cache.insert(slot_key.clone(), entry);

    ctx.track_dep(slot_key);
    (value, ts)
}

/// B2: Evaluate with purity tracking
/// Returns (value, timestamp, is_pure)
/// Pure expressions don't depend on events/HOLD and can be cached across ticks
fn eval_inner_with_purity(expr: &Expr, scope: &ScopeId, ctx: &mut EvalContext) -> (Value, TickSeq, bool) {
    let ts = ctx.tick_counter.next();

    match &expr.kind {
        ExprKind::Literal(lit) => {
            let value = literal_to_value(lit);
            (value, ts, true) // Literals are always pure
        }

        ExprKind::Variable(name) => {
            if let Some((var_scope, expr_id)) = ctx.lookup(name) {
                // Check if this is a HOLD state reference - NOT pure
                let slot_key = SlotKey::new(var_scope.clone(), expr_id);
                if let Some(cell) = ctx.holds.get(&slot_key) {
                    return (cell.value.clone(), ts, false);
                }

                // Check if value is cached (for lambda bindings like List/retain item)
                if let Some(entry) = ctx.cache.get(&slot_key) {
                    return (entry.value.clone(), entry.last_changed, entry.is_pure);
                }

                // Find the expression in the program
                if let Some((_, var_expr)) = ctx.program.bindings.iter().find(|(_, e)| e.id == expr_id) {
                    let (val, vts) = eval(var_expr, &var_scope, ctx);
                    // Variable purity depends on what it references
                    let is_pure = ctx.cache.get(&SlotKey::new(var_scope, expr_id))
                        .map(|e| e.is_pure)
                        .unwrap_or(false);
                    return (val, vts, is_pure);
                }
            }
            (Value::Skip, ts, false)
        }

        ExprKind::Path(base, field) => {
            let (base_val, base_ts) = eval(base, scope, ctx);
            let value = ops::get_field(&base_val, field);
            // Path is pure if base is pure
            let base_key = SlotKey::new(scope.clone(), base.id);
            let is_pure = ctx.cache.get(&base_key).map(|e| e.is_pure).unwrap_or(false);
            (value, base_ts, is_pure)
        }

        ExprKind::Object(fields) => {
            let mut obj = HashMap::new();
            let mut max_ts = ts;
            let mut all_pure = true;
            for (name, field_expr) in fields {
                let (val, field_ts) = eval(field_expr, scope, ctx);
                obj.insert(name.clone(), val);
                if field_ts > max_ts {
                    max_ts = field_ts;
                }
                // Check if this field is pure
                let field_key = SlotKey::new(scope.clone(), field_expr.id);
                if !ctx.cache.get(&field_key).map(|e| e.is_pure).unwrap_or(false) {
                    all_pure = false;
                }
            }
            (Value::Object(std::sync::Arc::new(obj)), max_ts, all_pure)
        }

        ExprKind::List(items) => {
            let mut list = Vec::new();
            let mut max_ts = ts;
            let mut all_pure = true;
            for item_expr in items {
                let (val, item_ts) = eval(item_expr, scope, ctx);
                list.push(val);
                if item_ts > max_ts {
                    max_ts = item_ts;
                }
                let item_key = SlotKey::new(scope.clone(), item_expr.id);
                if !ctx.cache.get(&item_key).map(|e| e.is_pure).unwrap_or(false) {
                    all_pure = false;
                }
            }
            (Value::List(std::sync::Arc::new(list)), max_ts, all_pure)
        }

        ExprKind::Call(name, args) => {
            let mut arg_values = Vec::new();
            let mut max_ts = ts;
            let mut all_pure = is_pure_function(name);
            for arg in args {
                let (val, arg_ts) = eval(arg, scope, ctx);
                arg_values.push(val);
                if arg_ts > max_ts {
                    max_ts = arg_ts;
                }
                let arg_key = SlotKey::new(scope.clone(), arg.id);
                if !ctx.cache.get(&arg_key).map(|e| e.is_pure).unwrap_or(false) {
                    all_pure = false;
                }
            }
            let result = call_builtin(name, &arg_values);
            (result, max_ts, all_pure)
        }

        ExprKind::Pipe(input, method, args) => {
            let (input_val, input_ts) = eval(input, scope, ctx);
            let mut arg_values = vec![input_val];
            let mut max_ts = input_ts;
            let input_key = SlotKey::new(scope.clone(), input.id);
            let mut all_pure = is_pure_function(method)
                && ctx.cache.get(&input_key).map(|e| e.is_pure).unwrap_or(false);
            for arg in args {
                let (val, arg_ts) = eval(arg, scope, ctx);
                arg_values.push(val);
                if arg_ts > max_ts {
                    max_ts = arg_ts;
                }
                let arg_key = SlotKey::new(scope.clone(), arg.id);
                if !ctx.cache.get(&arg_key).map(|e| e.is_pure).unwrap_or(false) {
                    all_pure = false;
                }
            }
            let result = call_builtin(method, &arg_values);
            (result, max_ts, all_pure)
        }

        ExprKind::Latest(exprs) => {
            // LATEST can depend on events - not pure
            let mut result = Value::Skip;
            let mut max_ts = ts;
            for sub_expr in exprs {
                let (val, sub_ts) = eval(sub_expr, scope, ctx);
                if !is_skip(&val) {
                    result = val;
                    max_ts = sub_ts;
                }
            }
            (result, max_ts, false)
        }

        ExprKind::Hold { initial, state_name, body } => {
            // HOLD is stateful - never pure
            let slot_key = SlotKey::new(scope.clone(), expr.id);

            let current_value = if let Some(cell) = ctx.holds.get(&slot_key) {
                cell.value.clone()
            } else {
                let (init_val, _) = eval(initial, scope, ctx);
                ctx.holds.insert(slot_key.clone(), HoldCell::new(init_val.clone()));
                ctx.holds_by_scope
                    .entry(scope.clone())
                    .or_default()
                    .push(slot_key.clone());
                init_val
            };

            ctx.bind(state_name.clone(), scope.clone(), expr.id);
            let (body_val, body_ts) = eval(body, scope, ctx);

            if !is_skip(&body_val) {
                if let Some(cell) = ctx.holds.get_mut(&slot_key) {
                    cell.value = body_val.clone();
                }
                (body_val, body_ts, false)
            } else {
                (current_value, ts, false)
            }
        }

        ExprKind::Then { input, body } => {
            // THEN depends on events - not pure
            let (input_val, _) = eval(input, scope, ctx);
            if is_skip(&input_val) {
                (Value::Skip, ts, false)
            } else {
                let (val, vts) = eval(body, scope, ctx);
                (val, vts, false)
            }
        }

        ExprKind::When { input, arms } => {
            // WHEN depends on events - not pure
            let (input_val, _) = eval(input, scope, ctx);
            if is_skip(&input_val) {
                return (Value::Skip, ts, false);
            }

            for (pattern, body) in arms {
                if pattern_matches(pattern, &input_val, &mut ctx.bindings, scope) {
                    let (val, vts) = eval(body, scope, ctx);
                    return (val, vts, false);
                }
            }
            (Value::Skip, ts, false)
        }

        ExprKind::While { input, pattern, body } => {
            // WHILE depends on events - not pure
            let (input_val, _) = eval(input, scope, ctx);
            if is_skip(&input_val) {
                return (Value::Skip, ts, false);
            }

            if pattern_matches(pattern, &input_val, &mut ctx.bindings, scope) {
                let (val, vts) = eval(body, scope, ctx);
                (val, vts, false)
            } else {
                (Value::Skip, ts, false)
            }
        }

        ExprKind::Link(_alias) => {
            // LINK is event-driven - never pure
            let slot_key = SlotKey::new(scope.clone(), expr.id);
            let cell = ctx.links.entry(slot_key.clone()).or_insert_with(LinkCell::new);

            if let Some(event) = cell.peek_event() {
                (event.clone(), ctx.tick_counter.next(), false)
            } else {
                (Value::Skip, ts, false)
            }
        }

        ExprKind::Block { bindings, output } => {
            let mut all_pure = true;
            for (name, binding_expr) in bindings {
                let (val, _) = eval(binding_expr, scope, ctx);
                ctx.bind(name.clone(), scope.clone(), binding_expr.id);
                let binding_key = SlotKey::new(scope.clone(), binding_expr.id);
                let is_binding_pure = ctx.cache.get(&binding_key).map(|e| e.is_pure).unwrap_or(false);
                if !is_binding_pure {
                    all_pure = false;
                }
                ctx.cache.insert(binding_key, CacheEntry::new(val, ctx.current_tick(), ts));
            }

            let (val, vts) = eval(output, scope, ctx);
            let output_key = SlotKey::new(scope.clone(), output.id);
            if !ctx.cache.get(&output_key).map(|e| e.is_pure).unwrap_or(false) {
                all_pure = false;
            }
            (val, vts, all_pure)
        }

        ExprKind::ListMap { list, item_name, template } => {
            // ListMap is dynamic - not pure
            let _slot_key = SlotKey::new(scope.clone(), expr.id);
            let (list_val, _) = eval(list, scope, ctx);

            match list_val {
                Value::List(items) => {
                    let mut results = Vec::new();
                    for (idx, item) in items.iter().enumerate() {
                        let item_scope = scope.child(idx as u64);
                        ctx.bind(item_name.clone(), item_scope.clone(), template.id);
                        let item_key = SlotKey::new(item_scope.clone(), template.id);
                        ctx.cache.insert(item_key, CacheEntry::new(item.clone(), ctx.current_tick(), ts));
                        let (result, _) = eval(template, &item_scope, ctx);
                        results.push(result);
                    }
                    (Value::List(std::sync::Arc::new(results)), ts, false)
                }
                _ => (Value::Skip, ts, false),
            }
        }

        ExprKind::ListAppend { list, item } => {
            // ListAppend is stateful - not pure
            let (list_val, _) = eval(list, scope, ctx);
            let list_slot = SlotKey::new(scope.clone(), expr.id);
            let item_key = {
                let list_cell = ctx.lists.entry(list_slot).or_insert_with(ListCell::new);
                list_cell.append()
            };
            let item_scope = scope.child(expr.id.0 as u64).child(item_key.0);
            let (item_val, _) = eval(item, &item_scope, ctx);
            let result = ops::list_append(&list_val, item_val);
            (result, ts, false)
        }

        ExprKind::ListClear { list } => {
            // ListClear is stateful - not pure
            let (list_val, _) = eval(list, scope, ctx);
            let list_slot = SlotKey::new(scope.clone(), list.id);
            // Clear the ListCell if it exists
            if let Some(list_cell) = ctx.lists.get_mut(&list_slot) {
                list_cell.clear();
            }
            // Return empty list
            let result = ops::list_clear(&list_val);
            (result, ts, false)
        }

        ExprKind::ListRemove { list, index } => {
            // ListRemove is stateful - not pure
            let (list_val, _) = eval(list, scope, ctx);
            let (index_val, _) = eval(index, scope, ctx);
            let list_slot = SlotKey::new(scope.clone(), list.id);

            if let Value::Int(idx) = index_val {
                // Remove from ListCell if it exists
                if let Some(list_cell) = ctx.lists.get_mut(&list_slot) {
                    list_cell.remove_at(idx as usize);
                }
                let result = ops::list_remove(&list_val, idx);
                (result, ts, false)
            } else {
                (list_val, ts, false)
            }
        }

        ExprKind::ListRetain { list, item_name, predicate } => {
            // ListRetain is stateful - not pure
            let (list_val, _) = eval(list, scope, ctx);

            match &list_val {
                Value::List(items) => {
                    // Evaluate predicate for each item using simple inline evaluation
                    let mut retain_indices = Vec::new();
                    for (idx, item) in items.iter().enumerate() {
                        // Evaluate predicate inline by substituting the item value
                        let pred_result = eval_predicate_inline(predicate, item_name, item);
                        if pred_result {
                            retain_indices.push(idx);
                        }
                    }

                    // Filter the list to only retained items
                    let retained: Vec<Value> = retain_indices
                        .iter()
                        .filter_map(|&idx| items.get(idx).cloned())
                        .collect();

                    // Update ListCell if it exists
                    let list_slot = SlotKey::new(scope.clone(), list.id);
                    if let Some(list_cell) = ctx.lists.get_mut(&list_slot) {
                        let retain_set: std::collections::HashSet<usize> = retain_indices.into_iter().collect();
                        list_cell.retain(|i| retain_set.contains(&i));
                    }

                    (Value::List(std::sync::Arc::new(retained)), ts, false)
                }
                _ => (list_val, ts, false),
            }
        }
    }
}

/// Evaluate a predicate inline by substituting the item value
/// Used for List/retain where we need to evaluate item => predicate patterns
fn eval_predicate_inline(predicate: &Expr, item_name: &str, item: &Value) -> bool {
    use shared::ast::ExprKind;
    match &predicate.kind {
        ExprKind::Pipe(base, method, _args) => {
            // Evaluate base, then apply method
            let base_val = eval_expr_inline(base, item_name, item);
            match method.as_str() {
                "Bool/not" => {
                    match base_val {
                        Value::Bool(b) => !b, // Bool/not returns the negation
                        _ => false,
                    }
                }
                _ => false,
            }
        }
        ExprKind::Path(base, field) => {
            let base_val = eval_expr_inline(base, item_name, item);
            match ops::get_field(&base_val, field) {
                Value::Bool(b) => b,
                _ => false,
            }
        }
        ExprKind::Variable(name) if name == item_name => {
            matches!(item, Value::Bool(true))
        }
        _ => false,
    }
}

/// Evaluate a simple expression inline by substituting the item value
fn eval_expr_inline(expr: &Expr, item_name: &str, item: &Value) -> Value {
    use shared::ast::ExprKind;
    match &expr.kind {
        ExprKind::Variable(name) if name == item_name => {
            item.clone()
        }
        ExprKind::Path(base, field) => {
            let base_val = eval_expr_inline(base, item_name, item);
            ops::get_field(&base_val, field)
        }
        _ => Value::Skip,
    }
}

/// Check if a function is pure (no side effects)
fn is_pure_function(name: &str) -> bool {
    matches!(
        name,
        "add" | "Bool/not" | "List/len" | "List/append" | "List/every" | "Math/sum"
    )
}

/// Convert a literal to a value
fn literal_to_value(lit: &Literal) -> Value {
    match lit {
        Literal::Int(v) => Value::Int(*v),
        Literal::Float(v) => Value::Float(*v),
        Literal::String(v) => Value::String(v.as_str().into()),  // String -> Arc<str>
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
        "List/clear" => {
            if !args.is_empty() {
                ops::list_clear(&args[0])
            } else {
                Value::Skip
            }
        }
        "List/remove" => {
            if args.len() >= 2 {
                if let Value::Int(idx) = &args[1] {
                    ops::list_remove(&args[0], *idx)
                } else {
                    Value::Skip
                }
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
