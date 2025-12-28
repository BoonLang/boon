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
    }

    /// Inject an event at a path
    fn inject_event(&mut self, path: &str, payload: Value) {
        // Parse path and find the target LINK
        let parts: Vec<&str> = path.split('.').collect();
        if parts.is_empty() {
            return;
        }

        // For now, find a LINK with matching path pattern
        // In a real implementation, we'd resolve the full path

        // Find top-level binding
        if let Some(&expr_id) = self.top_level.get(parts[0]) {
            let slot_key = SlotKey::new(ScopeId::root(), expr_id);

            // For simple paths like "button.click", inject to the link
            if let Some(link_cell) = self.links.get_mut(&slot_key) {
                link_cell.inject(payload);
            } else {
                // Create link cell and inject
                let mut link_cell = LinkCell::new();
                link_cell.inject(payload);
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

        // For HOLD cells, return the held value
        if let Some(cell) = self.holds.get(&slot_key) {
            value = cell.value.clone();
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
}
