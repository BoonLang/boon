//! General reactive interpreter for Boon programs.
//!
//! Handles ANY Boon program by evaluating the full AST reactively.
//! On each event (LINK click, timer tick, text input, router change),
//! updates reactive state and re-evaluates the document.
//!
//! This is in io/ because it uses Rc<RefCell<>> for mutable state.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use wasm_bindgen::prelude::Closure;
use wasm_bindgen::JsCast;
use zoon::*;

use super::super::core::value::Value;
use crate::parser::static_expression::{
    self, Alias, Argument, ArithmeticOperator, Arm, Expression, Literal, Pattern,
    Spanned, TextPart,
};

/// List mutation operations extracted from AST.
enum ListOp {
    Append { item_expr: Spanned<Expression>, on_expr: Option<Spanned<Expression>> },
    Clear { on_expr: Spanned<Expression> },
    Remove { item_var: String, on_expr: Spanned<Expression> },
}

/// A HOLD discovered during document evaluation (e.g., inside list items created by functions).
/// Stored so that `update_state` can process these HOLDs on subsequent events.
#[derive(Clone)]
struct DiscoveredHold {
    name: String,
    state_param: String,
    body: Spanned<Expression>,
    initial_expr: Spanned<Expression>,
    /// Scope bindings captured when the HOLD was discovered (sibling fields etc.)
    scope_bindings: Vec<(String, Value)>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

// Global tracking of active interval IDs so we can clear them when creating a new handle.
// This prevents timer leaks when re-running code (old timers from previous runs).
thread_local! {
    static ACTIVE_INTERVALS: RefCell<Vec<i32>> = RefCell::new(Vec::new());
}

fn clear_all_dd_intervals() {
    ACTIVE_INTERVALS.with(|intervals| {
        let ids = intervals.borrow().clone();
        for id in &ids {
            if let Some(window) = web_sys::window() {
                window.clear_interval_with_handle(*id);
            }
        }
        intervals.borrow_mut().clear();
    });
}

fn register_dd_interval(id: i32) {
    ACTIVE_INTERVALS.with(|intervals| {
        intervals.borrow_mut().push(id);
    });
}

// Reusable closure for auto-focusing elements with the autofocus attribute.
// Stored in a thread_local to avoid leaking closures on every render cycle.
// Uses JS eval to focus the LAST matching input[autofocus], because in edit mode
// both the main input and the edit input have autofocus, and the edit input
// (which appears later in the DOM) should take priority.
thread_local! {
    static AUTOFOCUS_CLOSURE: Closure<dyn FnMut()> = Closure::new(|| {
        // Use js_sys::eval to run querySelectorAll and focus last match,
        // avoiding web_sys feature requirements for NodeList.
        let _ = js_sys::eval(
            "var els = document.querySelectorAll('input[autofocus]'); \
             if (els.length > 0) els[els.length - 1].focus();"
        );
    });
}

/// Check if any text input in the preview panel currently has focus.
fn has_focused_input() -> bool {
    let result = js_sys::eval(
        "(function() { \
            var active = document.activeElement; \
            if (!active) return false; \
            var tag = active.tagName; \
            return tag === 'INPUT' || tag === 'TEXTAREA'; \
        })()"
    );
    result.map(|v| v.as_bool().unwrap_or(false)).unwrap_or(false)
}

fn schedule_autofocus() {
    AUTOFOCUS_CLOSURE.with(|c| {
        let _ = web_sys::window().unwrap()
            .set_timeout_with_callback(c.as_ref().unchecked_ref());
    });
}

/// A general reactive Boon program handle.
pub struct GeneralHandle {
    inner: Rc<RefCell<GeneralInner>>,
}

struct GeneralInner {
    evaluator: GeneralEvaluator,
    state: ProgramState,
    output: Mutable<Value>,
    timer_handles: Vec<TimerHandle>,
    storage_key: Option<String>,
    /// Track the last blur link_path to suppress spurious re-render-induced blur loops.
    /// When we re-render after a blur, the old input element is destroyed → triggers another blur.
    /// We suppress the second blur to break the cycle.
    last_blur_path: Option<String>,
}

struct TimerHandle {
    interval_id: i32,
    _closure: Closure<dyn FnMut()>,
}

impl Drop for TimerHandle {
    fn drop(&mut self) {
        if let Some(window) = web_sys::window() {
            window.clear_interval_with_handle(self.interval_id);
        }
    }
}

/// The program state (holds, lists, router, text inputs, etc.)
#[derive(Clone, Debug)]
pub struct ProgramState {
    pub holds: BTreeMap<String, Value>,
    pub lists: BTreeMap<String, Vec<Value>>,
    pub router_path: String,
    pub text_inputs: BTreeMap<String, String>,
    pub hovered: BTreeMap<String, bool>,
    /// Events fired in the current evaluation cycle
    pub fired_events: BTreeMap<String, Value>,
    /// List append counter for unique keys
    pub list_counters: BTreeMap<String, usize>,
    /// Math/sum accumulators (variable name → current sum)
    pub sum_accumulators: BTreeMap<String, f64>,
    /// Stream/skip counters (variable name → events skipped so far)
    pub skip_counts: BTreeMap<String, usize>,
    /// Timer tick counts for each timer variable
    pub timer_tick_counts: BTreeMap<String, usize>,
    /// Current variable being evaluated (for timer detection)
    pub current_eval_var: Option<String>,
}

impl ProgramState {
    fn new() -> Self {
        // Get current browser path
        let router_path = web_sys::window()
            .and_then(|w| w.location().pathname().ok())
            .unwrap_or_else(|| "/".to_string());
        Self {
            holds: BTreeMap::new(),
            lists: BTreeMap::new(),
            router_path,
            text_inputs: BTreeMap::new(),
            hovered: BTreeMap::new(),
            fired_events: BTreeMap::new(),
            list_counters: BTreeMap::new(),
            sum_accumulators: BTreeMap::new(),
            skip_counts: BTreeMap::new(),
            timer_tick_counts: BTreeMap::new(),
            current_eval_var: None,
        }
    }
}

/// Event types that can be fired
#[derive(Clone, Debug)]
pub enum Event {
    LinkPress { link_path: String },
    LinkClick { link_path: String },
    KeyDown { link_path: String, key: String },
    TextChange { link_path: String, text: String },
    Blur { link_path: String },
    Focus { link_path: String },
    DoubleClick { link_path: String },
    HoverChange { link_path: String, hovered: bool },
    TimerTick { var_name: String },
    RouterChange { path: String },
}

impl GeneralHandle {
    pub fn new(
        variables: Vec<(String, Spanned<Expression>)>,
        functions: Vec<(String, Vec<String>, Spanned<Expression>)>,
        output: Mutable<Value>,
        states_storage_key: Option<&str>,
    ) -> Self {
        // Clear any timers from previous runs to prevent leaks
        clear_all_dd_intervals();

        let evaluator = GeneralEvaluator::new(variables, functions);
        let mut state = ProgramState::new();

        // Load persisted state if available
        if let Some(key) = states_storage_key {
            Self::load_persisted_state(&mut state, key);
        }

        // Initialize lists BEFORE document eval so that list items get proper
        // per-item HOLD prefixes. Only initializes lists — does NOT process HOLDs,
        // accumulators, or side effects (those happen on the first event).
        evaluator.initialize_lists(&mut state);

        // Initial evaluation
        let doc = evaluator.evaluate_document(&state);
        output.set(doc);

        let inner = GeneralInner {
            evaluator,
            state,
            output,
            timer_handles: Vec::new(),
            storage_key: states_storage_key.map(|s| s.to_string()),
            last_blur_path: None,
        };

        let handle = GeneralHandle {
            inner: Rc::new(RefCell::new(inner)),
        };

        // Set up timers
        handle.setup_timers();

        // Schedule autofocus for any input with focus: True
        schedule_autofocus();

        handle
    }

    fn load_persisted_state(state: &mut ProgramState, key: &str) {
        if let Ok(Some(storage)) = web_sys::window().unwrap().local_storage() {
            // Load holds
            if let Ok(Some(json)) = storage.get_item(&format!("dd_{}_holds", key)) {
                if let Ok(holds) = serde_json::from_str::<BTreeMap<String, Value>>(&json) {
                    state.holds = holds;
                }
            }
            // Load lists
            if let Ok(Some(json)) = storage.get_item(&format!("dd_{}_lists", key)) {
                if let Ok(lists) = serde_json::from_str::<BTreeMap<String, Vec<Value>>>(&json) {
                    state.lists = lists;
                }
            }
            // Load list counters
            if let Ok(Some(json)) = storage.get_item(&format!("dd_{}_list_counters", key)) {
                if let Ok(counters) = serde_json::from_str::<BTreeMap<String, usize>>(&json) {
                    state.list_counters = counters;
                }
            }
            // Load sum accumulators
            if let Ok(Some(json)) = storage.get_item(&format!("dd_{}_sums", key)) {
                if let Ok(sums) = serde_json::from_str::<BTreeMap<String, f64>>(&json) {
                    state.sum_accumulators = sums;
                }
            }
        }
    }

    pub fn save_state(&self, key: &str) {
        let inner = self.inner.borrow();
        if let Ok(Some(storage)) = web_sys::window().unwrap().local_storage() {
            if let Ok(json) = serde_json::to_string(&inner.state.holds) {
                let _ = storage.set_item(&format!("dd_{}_holds", key), &json);
            }
            if let Ok(json) = serde_json::to_string(&inner.state.lists) {
                let _ = storage.set_item(&format!("dd_{}_lists", key), &json);
            }
            if let Ok(json) = serde_json::to_string(&inner.state.list_counters) {
                let _ = storage.set_item(&format!("dd_{}_list_counters", key), &json);
            }
        }
    }

    fn setup_timers(&self) {
        let inner_ref = self.inner.borrow();
        let timer_vars: Vec<(String, f64)> = inner_ref.evaluator.find_timer_vars();
        drop(inner_ref);

        for (var_name, interval_secs) in timer_vars {
            let inner_weak = Rc::downgrade(&self.inner);
            let var = var_name.clone();
            let interval_ms = (interval_secs * 1000.0) as i32;
            let closure = Closure::wrap(Box::new(move || {
                if let Some(inner) = inner_weak.upgrade() {
                    inner.borrow_mut().handle_event(Event::TimerTick {
                        var_name: var.clone(),
                    });
                }
            }) as Box<dyn FnMut()>);
            let window = web_sys::window().unwrap();
            let interval_id = window
                .set_interval_with_callback_and_timeout_and_arguments_0(
                    closure.as_ref().unchecked_ref(),
                    interval_ms,
                )
                .unwrap();
            register_dd_interval(interval_id);
            self.inner.borrow_mut().timer_handles.push(TimerHandle {
                interval_id,
                _closure: closure,
            });
        }
    }

    pub fn inject_event(&self, event: Event) {
        self.inner.borrow_mut().handle_event(event);
    }

    pub fn current_output(&self) -> Value {
        self.inner.borrow().output.get_cloned()
    }

    pub fn clone_ref(&self) -> GeneralHandle {
        GeneralHandle {
            inner: self.inner.clone(),
        }
    }

    /// Get a reference to the evaluator for rendering
    pub(crate) fn get_evaluator(&self) -> Rc<RefCell<GeneralInner>> {
        self.inner.clone()
    }
}

impl GeneralInner {
    fn handle_event(&mut self, event: Event) {
        // Set up fired events for this evaluation cycle
        self.state.fired_events.clear();

        // Clear blur tracking only on meaningful user-initiated events.
        // HoverChange is not meaningful enough to reset blur suppression —
        // it fires continuously and would break the blur-loop guard.
        match &event {
            Event::LinkPress { .. }
            | Event::LinkClick { .. }
            | Event::KeyDown { .. }
            | Event::DoubleClick { .. } => {
                self.last_blur_path = None;
            }
            _ => {}
        }

        match &event {
            Event::LinkPress { link_path } => {
                self.state
                    .fired_events
                    .insert(format!("{}.event.press", link_path), Value::Unit);
            }
            Event::LinkClick { link_path } => {
                self.state
                    .fired_events
                    .insert(format!("{}.event.click", link_path), Value::Unit);
            }
            Event::KeyDown { link_path, key } => {
                // Skip re-render for regular character keys (single chars).
                // Only special keys (Enter, Escape, Tab, etc.) should trigger state updates.
                // Regular typing is handled via TextChange events.
                let is_special = key.len() > 1; // "Enter", "Escape", "Tab", etc.
                if !is_special {
                    return; // Skip re-render for regular character typing
                }
                self.state.fired_events.insert(
                    format!("{}.event.key_down", link_path),
                    Value::object([("key", Value::tag(key.as_str()))]),
                );
                self.state.fired_events.insert(
                    format!("{}.event.key_down.key", link_path),
                    Value::tag(key.as_str()),
                );
            }
            Event::TextChange { link_path, text } => {
                // Only update text_inputs — do NOT re-render on text change.
                // Re-rendering replaces the input element, killing focus and typed text.
                // The text value is available via text_inputs for LATEST/WHEN evaluation
                // when a meaningful event (key_down, blur) triggers re-render.
                self.state
                    .text_inputs
                    .insert(link_path.clone(), text.clone());
                return; // Skip update_state and re-render
            }
            Event::Blur { link_path } => {
                // Suppress spurious blur from re-rendering: when a blur triggers re-render,
                // the old input is destroyed → fires another blur. Suppress the duplicate.
                if self.last_blur_path.as_deref() == Some(link_path.as_str()) {
                    self.last_blur_path = None;
                    return;
                }
                self.last_blur_path = Some(link_path.clone());
                self.state
                    .fired_events
                    .insert(format!("{}.event.blur", link_path), Value::Unit);
            }
            Event::Focus { link_path } => {
                self.last_blur_path = None;
                self.state
                    .fired_events
                    .insert(format!("{}.event.focus", link_path), Value::Unit);
            }
            Event::DoubleClick { link_path } => {
                self.state
                    .fired_events
                    .insert(format!("{}.event.double_click", link_path), Value::Unit);
            }
            Event::HoverChange { link_path, hovered } => {
                self.state
                    .hovered
                    .insert(link_path.clone(), *hovered);
                // Note: The old full-rebuild approach skipped hover re-render when a text
                // input had focus or during editing, to prevent blur→hover→blur loops.
                // With the retained tree, DOM elements are NOT replaced — only Mutables
                // are updated — so the text input stays in the DOM and keeps focus.
                // No guard needed.
            }
            Event::TimerTick { var_name } => {
                self.state
                    .fired_events
                    .insert(format!("__timer__{}", var_name), Value::Unit);
                *self.state.timer_tick_counts.entry(var_name.clone()).or_insert(0) += 1;
            }
            Event::RouterChange { path } => {
                self.state.router_path = path.clone();
            }
        }

        // Evaluate HOLDs and LIST operations with the current events
        self.evaluator.update_state(&mut self.state);

        // Re-evaluate document (fired_events still available for LATEST detection)
        let doc = self.evaluator.evaluate_document(&self.state);
        self.output.set(doc);

        // Clear fired events AFTER document rendering
        self.state.fired_events.clear();

        // Persist state
        if let Some(ref key) = self.storage_key {
            Self::save_state_direct(&self.state, key);
        }

        // Schedule autofocus for any input with focus: True
        schedule_autofocus();
    }

    /// Check if any HOLD in state has a True value for an "editing" field.
    fn has_editing_hold(&self) -> bool {
        self.state.holds.iter().any(|(key, val)| {
            key.contains("editing") && val.as_bool() == Some(true)
        })
    }

    fn save_state_direct(state: &ProgramState, key: &str) {
        if super::super::is_save_disabled() {
            return;
        }
        if let Ok(Some(storage)) = web_sys::window().unwrap().local_storage() {
            if let Ok(json) = serde_json::to_string(&state.holds) {
                let _ = storage.set_item(&format!("dd_{}_holds", key), &json);
            }
            if let Ok(json) = serde_json::to_string(&state.lists) {
                let _ = storage.set_item(&format!("dd_{}_lists", key), &json);
            }
            if let Ok(json) = serde_json::to_string(&state.list_counters) {
                let _ = storage.set_item(&format!("dd_{}_list_counters", key), &json);
            }
            if let Ok(json) = serde_json::to_string(&state.sum_accumulators) {
                let _ = storage.set_item(&format!("dd_{}_sums", key), &json);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// General Evaluator (pure evaluation logic)
// ---------------------------------------------------------------------------

pub struct GeneralEvaluator {
    variables: Vec<(String, Spanned<Expression>)>,
    functions: Vec<(String, Vec<String>, Spanned<Expression>)>,
    /// Flag set when eval_hold_static completes a loop (Stream/pulses pattern).
    /// Stream/skip checks this to pass through the final result.
    static_hold_completed: Cell<bool>,
    /// Current variable being evaluated (for Math/sum accumulator lookup).
    current_eval_var: RefCell<Option<String>>,
    /// Current link path prefix — tracks the path for LINK values.
    /// Set when evaluating inside objects (for store LINKs) and by LinkSetter.
    current_link_prefix: RefCell<String>,
    /// HOLDs discovered during document evaluation (e.g., inside list items).
    /// These are registered so `update_state` can process them on events.
    discovered_holds: RefCell<Vec<DiscoveredHold>>,
}

impl GeneralEvaluator {
    pub fn new(
        variables: Vec<(String, Spanned<Expression>)>,
        functions: Vec<(String, Vec<String>, Spanned<Expression>)>,
    ) -> Self {
        Self {
            variables,
            functions,
            static_hold_completed: Cell::new(false),
            current_eval_var: RefCell::new(None),
            current_link_prefix: RefCell::new(String::new()),
            discovered_holds: RefCell::new(Vec::new()),
        }
    }

    /// Find timer variables (Duration[seconds: N] |> Timer/interval())
    pub fn find_timer_vars(&self) -> Vec<(String, f64)> {
        let mut timers = Vec::new();
        for (name, expr) in &self.variables {
            if let Some(secs) = self.extract_timer_interval(expr) {
                timers.push((name.clone(), secs));
            }
        }
        timers
    }

    fn extract_timer_interval(&self, expr: &Spanned<Expression>) -> Option<f64> {
        match &expr.node {
            Expression::Pipe { from, to } => {
                if let Expression::FunctionCall { path, .. } = &to.node {
                    let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                    if path_strs.as_slice() == &["Timer", "interval"] {
                        return self.extract_duration_seconds(from);
                    }
                }
                self.extract_timer_interval(from)
                    .or_else(|| self.extract_timer_interval(to))
            }
            Expression::Latest { inputs } => {
                inputs.iter().find_map(|i| self.extract_timer_interval(i))
            }
            // Don't follow aliases — only detect timers directly in this variable's expression.
            // Following aliases would create duplicate timers (e.g., both "tick" and "counter"
            // would get timers in interval_hold.bn).
            _ => None,
        }
    }

    fn extract_duration_seconds(&self, expr: &Spanned<Expression>) -> Option<f64> {
        match &expr.node {
            Expression::TaggedObject { tag, object } => {
                if tag.as_str() == "Duration" {
                    for var in &object.variables {
                        if var.node.name.as_str() == "seconds" {
                            if let Expression::Literal(Literal::Number(n)) = &var.node.value.node {
                                return Some(*n);
                            }
                        }
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Handle Router/go_to side effect during update_state.
    /// Extracts event paths from LATEST branches, prefixes with parent scope,
    /// and navigates when a matching event is found.
    fn handle_router_go_to(
        &self,
        name: &str,
        from: &Spanned<Expression>,
        state: &mut ProgramState,
    ) {
        // Compute parent prefix from variable name.
        // e.g., "store.nav_action" → prefix "store."
        let parent_prefix = if let Some(dot_pos) = name.rfind('.') {
            &name[..dot_pos + 1]
        } else {
            ""
        };

        if let Expression::Latest { inputs } = &from.node {
            for input in inputs {
                if let Expression::Pipe {
                    from: event_expr,
                    to: then_expr,
                } = &input.node
                {
                    if let Expression::Then { body } = &then_expr.node {
                        // Extract the alias path from the event expression
                        if let Some(event_path) = Self::extract_alias_path(event_expr) {
                            let full_path = format!("{}{}", parent_prefix, event_path);
                            if state.fired_events.contains_key(&full_path) {
                                // Evaluate the THEN body to get the navigation path
                                if let Ok(val) = self.eval(body, &[], state, None) {
                                    if let Some(path) = val.as_text() {
                                        if path != state.router_path.as_str() {
                                            state.router_path = path.to_string();
                                            // Push to browser history
                                            if let Some(window) = web_sys::window() {
                                                if let Ok(history) = window.history() {
                                                    let _ = history.push_state_with_url(
                                                        &wasm_bindgen::JsValue::NULL,
                                                        "",
                                                        Some(path),
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                                break; // Only one event should match per cycle
                            }
                        }
                    }
                }
            }
        }
    }

    /// Extract a dot-separated alias path from an expression.
    fn extract_alias_path(expr: &Spanned<Expression>) -> Option<String> {
        match &expr.node {
            Expression::Alias(Alias::WithoutPassed { parts, .. }) => Some(
                parts
                    .iter()
                    .map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join("."),
            ),
            _ => None,
        }
    }

    /// Initialize lists from their expressions so that per-item HOLDs
    /// get correct prefixed names. Called once in GeneralHandle::new() before
    /// the first evaluate_document(). Does NOT process HOLDs, accumulators, or
    /// side effects — only initializes list contents.
    pub fn initialize_lists(&self, state: &mut ProgramState) {
        for (name, expr) in &self.variables {
            if name.contains('.') {
                if let Some(dot_pos) = name.rfind('.') {
                    let parent = &name[..dot_pos];
                    if self.variables.iter().any(|(n, _)| n == parent) {
                        continue;
                    }
                }
            }
            *self.current_eval_var.borrow_mut() = Some(name.clone());
            self.initialize_lists_in_expr(name, expr, state);
        }
        *self.current_eval_var.borrow_mut() = None;
    }

    fn initialize_lists_in_expr(&self, name: &str, expr: &Spanned<Expression>, state: &mut ProgramState) {
        match &expr.node {
            Expression::Pipe { from, to } => {
                // Check if this pipe chain contains list operations
                let ops = self.extract_list_operations(name, expr);
                if !ops.is_empty() {
                    // Always evaluate list items to discover per-item HOLDs
                    // (needed even when list is loaded from persistence).
                    let initial = self.extract_list_initial(name, expr, state);
                    if !state.lists.contains_key(name) {
                        let count = initial.len();
                        state.list_counters.entry(name.to_string()).or_insert(count);
                        state.lists.insert(name.to_string(), initial);
                    }
                } else {
                    // Recurse into from
                    self.initialize_lists_in_expr(name, from, state);
                }
            }
            Expression::Object(obj) => {
                for var in &obj.variables {
                    let field_name = format!("{}.{}", name, var.node.name.as_str());
                    let prev = self.current_eval_var.borrow().clone();
                    *self.current_eval_var.borrow_mut() = Some(field_name.clone());
                    self.initialize_lists_in_expr(&field_name, &var.node.value, state);
                    *self.current_eval_var.borrow_mut() = prev;
                }
            }
            _ => {}
        }
    }

    /// Update program state based on current events.
    /// Evaluates all HOLD bodies, LIST operations, and accumulators.
    pub fn update_state(&self, state: &mut ProgramState) {
        // Process variables in order, looking for HOLDs, LISTs, and accumulators.
        // Skip flattened object fields (dotted names whose parent is also a variable)
        // because those are already processed when we recurse into the parent Object.
        for (name, expr) in &self.variables {
            if name.contains('.') {
                // Check if parent object is also a variable — if so, skip this flattened field
                if let Some(dot_pos) = name.rfind('.') {
                    let parent = &name[..dot_pos];
                    if self.variables.iter().any(|(n, _)| n == parent) {
                        continue;
                    }
                }
            }
            *self.current_eval_var.borrow_mut() = Some(name.clone());
            self.update_var_state(name, expr, state);
        }
        *self.current_eval_var.borrow_mut() = None;

        // Process HOLDs discovered during document evaluation (e.g., inside list items).
        // These were registered by eval_pipe when encountering HOLDs with per-item keys.
        let discovered = self.discovered_holds.borrow().clone();
        for hold in &discovered {
            *self.current_eval_var.borrow_mut() = Some(hold.name.clone());
            *self.current_link_prefix.borrow_mut() = hold.name.clone();
            // Build scope from captured bindings — convert String keys to &str
            // using the same leak technique used elsewhere for scope lifetimes
            let extra_scope: Vec<(&str, Value)> = hold.scope_bindings.iter().map(|(k, v)| {
                let k_owned: String = k.clone();
                let k_leaked: &str = unsafe { &*(k_owned.as_str() as *const str) };
                std::mem::forget(k_owned);
                (k_leaked, v.clone())
            }).collect();
            self.update_hold_state_with_scope(
                &hold.name,
                &hold.state_param,
                &hold.body,
                &hold.initial_expr,
                state,
                &extra_scope,
            );
            *self.current_link_prefix.borrow_mut() = String::new();
        }
        *self.current_eval_var.borrow_mut() = None;

        // Sync per-item HOLD values back to list items in state.lists.
        // Hold names like "store.todos.0003.completed" encode the item prefix and field name.
        // We match items by their embedded __item_prefix__ (monotonic index, not position).
        for (hold_name, hold_val) in &state.holds {
            let segments: Vec<&str> = hold_name.split('.').collect();
            // Look for a 4-digit numeric segment indicating a per-item hold
            for (i, seg) in segments.iter().enumerate() {
                if seg.len() == 4 && seg.chars().all(|c| c.is_ascii_digit()) && i + 1 < segments.len() {
                    let list_name = segments[..i].join(".");
                    let item_prefix = segments[..=i].join(".");
                    let field_name = segments[i + 1..].join(".");
                    if let Some(list) = state.lists.get_mut(&list_name) {
                        // Find item by __item_prefix__ instead of position
                        for item in list.iter_mut() {
                            if let Value::Object(obj) = &*item {
                                if let Some(prefix_val) = obj.get("__item_prefix__" as &str) {
                                    if prefix_val.as_text() == Some(&item_prefix) {
                                        let mut new_fields = (**obj).clone();
                                        new_fields.insert(Arc::from(field_name.as_str()), hold_val.clone());
                                        *item = Value::Object(Arc::new(new_fields));
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    break;
                }
            }
        }
    }

    fn update_var_state(&self, name: &str, expr: &Spanned<Expression>, state: &mut ProgramState) {
        match &expr.node {
            Expression::Pipe { from, to } => {
                match &to.node {
                    Expression::Hold { state_param, body } => {
                        self.update_hold_state(name, state_param.as_str(), body, from, state);
                    }
                    Expression::FunctionCall { path, arguments } => {
                        let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                        match path_strs.as_slice() {
                            ["Math", "sum"] => {
                                // Math/sum accumulator: evaluate pipe input and add to sum
                                let input_val = self.eval(from, &[], state, None);
                                if let Ok(val) = input_val {
                                    if !self.is_skip(&val) {
                                        let add = val.as_number().unwrap_or(0.0);
                                        let sum = state.sum_accumulators.entry(name.to_string()).or_insert(0.0);
                                        *sum += add;
                                    }
                                }
                            }
                            ["Router", "go_to"] => {
                                // Navigation side effect: check LATEST branches for fired events
                                self.handle_router_go_to(name, from, state);
                            }
                            ["Stream", "skip"] => {
                                // Stream/skip(count: N) — track skip counts, pass through to inner
                                self.update_var_state(name, from, state);
                                // Increment skip count on each event
                                let skip_key = format!("__skip__");
                                *state.skip_counts.entry(skip_key).or_insert(0) += 1;
                            }
                            ["Document", "new"] | ["Log", "info"] => {
                                // Pass through to inner
                                self.update_var_state(name, from, state);
                            }
                            _ => {
                                // Check if this is a LIST with operations
                                let ops = self.extract_list_operations(name, expr);
                                if !ops.is_empty() {
                                    self.update_list_state(name, expr, state);
                                    // Don't recurse — update_list_state handles the full chain
                                } else {
                                    // Check nested pipes
                                    self.update_var_state(name, from, state);
                                }
                            }
                        }
                    }
                    _ => {
                        // Check if this is a LIST with operations
                        let ops = self.extract_list_operations(name, expr);
                        if !ops.is_empty() {
                            self.update_list_state(name, expr, state);
                            // Don't recurse — update_list_state handles the full chain
                        } else {
                            // Check nested pipes
                            self.update_var_state(name, from, state);
                        }
                    }
                }
            }
            Expression::Object(obj) => {
                // Check fields for reactive constructs
                for var in &obj.variables {
                    let field_name = format!("{}.{}", name, var.node.name.as_str());
                    let prev = self.current_eval_var.borrow().clone();
                    *self.current_eval_var.borrow_mut() = Some(field_name.clone());
                    self.update_var_state(&field_name, &var.node.value, state);
                    *self.current_eval_var.borrow_mut() = prev;
                }
            }
            _ => {}
        }
    }

    fn update_hold_state(
        &self,
        hold_name: &str,
        state_param: &str,
        body: &Spanned<Expression>,
        initial_expr: &Spanned<Expression>,
        state: &mut ProgramState,
    ) {
        self.update_hold_state_with_scope(hold_name, state_param, body, initial_expr, state, &[])
    }

    fn update_hold_state_with_scope(
        &self,
        hold_name: &str,
        state_param: &str,
        body: &Spanned<Expression>,
        initial_expr: &Spanned<Expression>,
        state: &mut ProgramState,
        extra_scope: &[(&str, Value)],
    ) {
        // Initialize if not yet in state
        if !state.holds.contains_key(hold_name) {
            let initial = self.eval(initial_expr, extra_scope, state, None).unwrap_or(Value::Unit);
            state.holds.insert(hold_name.to_string(), initial);
        }

        // Evaluate the HOLD body with current state and events
        let current = state.holds.get(hold_name).cloned().unwrap_or(Value::Unit);
        let mut scope: Vec<(&str, Value)> = extra_scope.to_vec();
        let name_owned: String = state_param.to_string();
        let name_leaked: &str = unsafe { &*(name_owned.as_str() as *const str) };
        std::mem::forget(name_owned);
        scope.push((name_leaked, current.clone()));

        let result = self.eval(body, &scope, state, None);
        if let Ok(ref new_val) = result {
            if !self.is_skip(new_val) {
                state.holds.insert(hold_name.to_string(), new_val.clone());
            }
        }
    }

    fn update_list_state(
        &self,
        name: &str,
        expr: &Spanned<Expression>,
        state: &mut ProgramState,
    ) {
        // Look for LIST {} |> List/append(...) |> List/clear(...) etc.
        let ops = self.extract_list_operations(name, expr);
        if ops.is_empty() {
            return;
        }

        // Initialize list if needed
        if !state.lists.contains_key(name) {
            let initial = self.extract_list_initial(name, expr, state);
            // Set monotonic counter past initial items so appends get unique indices
            let count = initial.len();
            state.list_counters.entry(name.to_string()).or_insert(count);
            state.lists.insert(name.to_string(), initial);
        }

        // Apply operations
        for op in ops {
            match op {
                ListOp::Append { item_expr, on_expr } => {
                    // Check the `on:` condition first (if present).
                    // Only append when the condition produces a non-SKIP value.
                    if let Some(ref on) = on_expr {
                        let on_val = self.eval(on, &[], state, None);
                        match on_val {
                            Ok(ref v) if self.is_skip(v) => continue,
                            Err(_) => continue,
                            _ => {}
                        }
                    }
                    // Use monotonic counter for unique per-item prefixes.
                    // list.len() would reuse indices after removals, causing stale hold conflicts.
                    let next_idx = state.list_counters.entry(name.to_string()).or_insert(0);
                    let idx = *next_idx;
                    *next_idx = idx + 1;
                    let old_prefix = self.current_link_prefix.borrow().clone();
                    let item_prefix = format!("{}.{:04}", name, idx);
                    *self.current_link_prefix.borrow_mut() = item_prefix.clone();
                    let item = self.eval(&item_expr, &[], state, None);
                    *self.current_link_prefix.borrow_mut() = old_prefix;
                    if let Ok(val) = item {
                        if !self.is_skip(&val) {
                            // Embed the per-item prefix for HOLD sync
                            let val = Self::embed_item_prefix(val, &item_prefix);
                            let list = state.lists.entry(name.to_string()).or_default();
                            list.push(val);
                        }
                    }
                }
                ListOp::Clear { on_expr } => {
                    let on = self.eval(&on_expr, &[], state, None);
                    if let Ok(val) = on {
                        if !self.is_skip(&val) {
                            state.lists.insert(name.to_string(), Vec::new());
                        }
                    }
                }
                ListOp::Remove {
                    item_var,
                    on_expr,
                } => {
                    let list = state.lists.get(name).cloned().unwrap_or_default();
                    let mut to_remove = Vec::new();
                    for (i, item) in list.iter().enumerate() {
                        let scope = vec![(item_var.as_str(), item.clone())];
                        if let Ok(val) = self.eval(&on_expr, &scope, state, None) {
                            if !self.is_skip(&val) {
                                to_remove.push(i);
                            }
                        }
                    }
                    if !to_remove.is_empty() {
                        let mut new_list = Vec::new();
                        for (i, item) in list.into_iter().enumerate() {
                            if !to_remove.contains(&i) {
                                new_list.push(item);
                            }
                        }
                        state.lists.insert(name.to_string(), new_list);
                    }
                }
            }
        }
    }

    fn extract_list_initial(
        &self,
        list_name: &str,
        expr: &Spanned<Expression>,
        state: &ProgramState,
    ) -> Vec<Value> {
        match &expr.node {
            Expression::Pipe { from, .. } => self.extract_list_initial(list_name, from, state),
            Expression::List { items } => {
                let mut result = Vec::new();
                for (i, item) in items.iter().enumerate() {
                    // Set per-item link prefix so HOLDs inside list items get unique keys
                    let old_prefix = self.current_link_prefix.borrow().clone();
                    let item_prefix = format!("{}.{:04}", list_name, i);
                    *self.current_link_prefix.borrow_mut() = item_prefix.clone();
                    if let Ok(val) = self.eval(item, &[], state, None) {
                        result.push(Self::embed_item_prefix(val, &item_prefix));
                    }
                    *self.current_link_prefix.borrow_mut() = old_prefix;
                }
                result
            }
            _ => Vec::new(),
        }
    }

    fn extract_list_operations(
        &self,
        _name: &str,
        expr: &Spanned<Expression>,
    ) -> Vec<ListOp> {
        let mut ops = Vec::new();
        self.collect_list_ops(expr, &mut ops);
        ops
    }

    fn collect_list_ops(&self, expr: &Spanned<Expression>, ops: &mut Vec<ListOp>) {
        match &expr.node {
            Expression::Pipe { from, to } => {
                // Check if `to` is a List operation
                if let Expression::FunctionCall { path, arguments } = &to.node {
                    let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                    match path_strs.as_slice() {
                        ["List", "append"] => {
                            if let Some(item_arg) =
                                arguments.iter().find(|a| a.node.name.as_str() == "item")
                            {
                                if let Some(ref val_expr) = item_arg.node.value {
                                    let on_expr = arguments
                                        .iter()
                                        .find(|a| a.node.name.as_str() == "on")
                                        .and_then(|a| a.node.value.clone());
                                    ops.push(ListOp::Append {
                                        item_expr: val_expr.clone(),
                                        on_expr,
                                    });
                                }
                            }
                        }
                        ["List", "clear"] => {
                            if let Some(on_arg) =
                                arguments.iter().find(|a| a.node.name.as_str() == "on")
                            {
                                if let Some(ref val_expr) = on_arg.node.value {
                                    ops.push(ListOp::Clear {
                                        on_expr: val_expr.clone(),
                                    });
                                }
                            }
                        }
                        ["List", "remove"] => {
                            let item_var = arguments
                                .iter()
                                .find(|a| a.node.name.as_str() == "item" && a.node.value.is_none())
                                .map(|a| a.node.name.as_str().to_string())
                                .unwrap_or_else(|| "item".to_string());
                            if let Some(on_arg) =
                                arguments.iter().find(|a| a.node.name.as_str() == "on")
                            {
                                if let Some(ref val_expr) = on_arg.node.value {
                                    ops.push(ListOp::Remove {
                                        item_var,
                                        on_expr: val_expr.clone(),
                                    });
                                }
                            }
                        }
                        _ => {}
                    }
                }
                // Recurse into from
                self.collect_list_ops(from, ops);
            }
            _ => {}
        }
    }

    /// Evaluate the document variable and produce the full document Value tree.
    pub fn evaluate_document(&self, state: &ProgramState) -> Value {
        let doc_expr = self
            .variables
            .iter()
            .find(|(n, _)| n == "document")
            .map(|(_, e)| e);

        match doc_expr {
            Some(expr) => {
                *self.current_eval_var.borrow_mut() = Some("document".to_string());
                let result = self.eval(expr, &[], state, None);
                *self.current_eval_var.borrow_mut() = None;
                if let Err(ref e) = result {
                    zoon::println!("[DD General] Document eval error: {}", e);
                }
                result.unwrap_or(Value::Unit)
            }
            None => Value::Unit,
        }
    }

    // -----------------------------------------------------------------------
    // Core evaluation function
    // -----------------------------------------------------------------------

    /// Evaluate an expression in the given scope and state.
    /// `passed` is the PASS/PASSED context value.
    pub fn eval(
        &self,
        expr: &Spanned<Expression>,
        scope: &[(&str, Value)],
        state: &ProgramState,
        passed: Option<&Value>,
    ) -> Result<Value, String> {
        match &expr.node {
            Expression::Literal(lit) => Ok(Self::eval_literal(lit)),

            Expression::TextLiteral { parts } => {
                let mut result = String::new();
                for part in parts {
                    match part {
                        TextPart::Text(s) => result.push_str(s.as_str()),
                        TextPart::Interpolation { var, .. } => {
                            let val = self.resolve_alias(var.as_str(), scope, state, passed)?;
                            result.push_str(&val.to_display_string());
                        }
                    }
                }
                Ok(Value::text(result))
            }

            Expression::Alias(alias) => self.eval_alias(alias, scope, state, passed),

            Expression::FunctionCall { path, arguments } => {
                self.eval_function_call(path, arguments, scope, state, passed, None)
            }

            Expression::Pipe { from, to } => {
                self.eval_pipe(from, to, scope, state, passed)
            }

            Expression::List { items } => {
                let mut fields = BTreeMap::new();
                for (i, item) in items.iter().enumerate() {
                    let val = self.eval(item, scope, state, passed)?;
                    fields.insert(Arc::from(format!("{:04}", i)), val);
                }
                Ok(Value::Tagged {
                    tag: Arc::from("List"),
                    fields: Arc::new(fields),
                })
            }

            Expression::Object(obj) => {
                let mut fields = BTreeMap::new();
                let mut obj_scope: Vec<(&str, Value)> = scope.to_vec();
                for var in &obj.variables {
                    let name_str = var.node.name.as_str();
                    // Extend link prefix for nested object fields
                    let old_prefix = self.current_link_prefix.borrow().clone();
                    let new_prefix = if old_prefix.is_empty() {
                        name_str.to_string()
                    } else {
                        format!("{}.{}", old_prefix, name_str)
                    };
                    *self.current_link_prefix.borrow_mut() = new_prefix.clone();

                    // Check if this field has stored state (LIST or HOLD)
                    // so we return the live state instead of re-evaluating the expression
                    let val = if let Some(items) = state.lists.get(&new_prefix) {
                        self.list_to_value(items)
                    } else if let Some(hold_val) = state.holds.get(&new_prefix) {
                        hold_val.clone()
                    } else if let Some(sum) = state.sum_accumulators.get(&new_prefix) {
                        Value::number(*sum)
                    } else {
                        self.eval(&var.node.value, &obj_scope, state, passed)?
                    };

                    *self.current_link_prefix.borrow_mut() = old_prefix;
                    let name_owned: String = name_str.to_string();
                    let name_leaked: &str =
                        unsafe { &*(name_owned.as_str() as *const str) };
                    std::mem::forget(name_owned);
                    obj_scope.push((name_leaked, val.clone()));
                    fields.insert(Arc::from(name_str), val);
                }
                Ok(Value::Object(Arc::new(fields)))
            }

            Expression::TaggedObject { tag, object } => {
                let mut fields = BTreeMap::new();
                for var in &object.variables {
                    let name = var.node.name.as_str().to_string();
                    let val = self.eval(&var.node.value, scope, state, passed)?;
                    fields.insert(Arc::from(name.as_str()), val);
                }
                Ok(Value::Tagged {
                    tag: Arc::from(tag.as_str()),
                    fields: Arc::new(fields),
                })
            }

            Expression::Block {
                variables, output, ..
            } => {
                let mut new_scope: Vec<(&str, Value)> = scope.to_vec();
                for var in variables {
                    let val = self.eval(&var.node.value, &new_scope, state, passed)?;
                    let name_str = var.node.name.as_str();
                    let already = new_scope.iter().position(|(n, _)| *n == name_str);
                    if let Some(idx) = already {
                        new_scope[idx].1 = val;
                    } else {
                        let name_owned: String = name_str.to_string();
                        let name_leaked: &str =
                            unsafe { &*(name_owned.as_str() as *const str) };
                        std::mem::forget(name_owned);
                        new_scope.push((name_leaked, val));
                    }
                }
                self.eval(output, &new_scope, state, passed)
            }

            Expression::ArithmeticOperator(op) => self.eval_arithmetic(op, scope, state, passed),

            Expression::Comparator(cmp) => self.eval_comparator(cmp, scope, state, passed),

            Expression::When { arms } => {
                // WHEN without pipe input — evaluate the first arm's pattern
                // This is used like: `WHEN { pattern => body }`
                // In practice, WHEN usually appears after a pipe
                Err("WHEN requires pipe input".to_string())
            }

            Expression::While { arms } => {
                // WHILE without pipe input
                Err("WHILE requires pipe input".to_string())
            }

            Expression::Link => {
                // LINK evaluates to a Tagged value with the current path embedded.
                let path = self.current_link_prefix.borrow().clone();
                if path.is_empty() {
                    Ok(Value::tag("LINK"))
                } else {
                    let mut fields = BTreeMap::new();
                    fields.insert(Arc::from("__path__"), Value::text(path.as_str()));
                    Ok(Value::Tagged {
                        tag: Arc::from("LINK"),
                        fields: Arc::new(fields),
                    })
                }
            }

            Expression::LinkSetter { alias } => {
                // LINK { alias } — in evaluation context, just return a marker
                Ok(Value::tag("LINK"))
            }

            Expression::Skip => Ok(Value::tag("SKIP")),

            Expression::Latest { inputs } => {
                self.eval_latest(inputs, scope, state, passed)
            }

            Expression::Hold { state_param, body } => {
                // HOLD without pipe — shouldn't happen at top level
                // This is handled in update_hold_state
                Err("HOLD requires pipe input".to_string())
            }

            Expression::Then { body } => {
                // THEN without pipe — shouldn't happen
                Err("THEN requires pipe input".to_string())
            }

            Expression::Function { .. } => {
                // Function definition — skip (already registered)
                Ok(Value::Unit)
            }

            Expression::Variable(var) => {
                // Variable definition — skip (already registered)
                Ok(Value::Unit)
            }

            Expression::Spread { value } => {
                self.eval(value, scope, state, passed)
            }

            _ => Err(format!(
                "Unsupported expression: {:?}",
                std::mem::discriminant(&expr.node)
            )),
        }
    }

    fn eval_literal(lit: &Literal) -> Value {
        match lit {
            Literal::Number(n) => Value::number(*n),
            Literal::Tag(t) => Value::tag(t.as_str()),
            Literal::Text(s) => Value::text(s.as_str()),
        }
    }

    fn eval_alias(
        &self,
        alias: &Alias,
        scope: &[(&str, Value)],
        state: &ProgramState,
        passed: Option<&Value>,
    ) -> Result<Value, String> {
        match alias {
            Alias::WithoutPassed { parts, .. } => {
                let name = parts[0].as_str();
                let mut val = self.resolve_alias(name, scope, state, passed)?;
                let mut i = 1;
                while i < parts.len() {
                    let part = parts[i].as_str();
                    // Handle LINK event/text/hovered access
                    if let Some(result) = self.try_link_field_access(&val, &parts[i..], state) {
                        return result;
                    }
                    val = val
                        .get_field(part)
                        .cloned()
                        .ok_or_else(|| format!("Field '{}' not found on {:?}", part, val))?;
                    i += 1;
                }
                Ok(val)
            }
            Alias::WithPassed { extra_parts } => {
                let passed_val = passed.cloned().ok_or("PASSED not available")?;
                let mut val = passed_val;
                let mut i = 0;
                while i < extra_parts.len() {
                    let part = extra_parts[i].as_str();
                    // Handle LINK event/text/hovered access
                    if let Some(result) = self.try_link_field_access(&val, &extra_parts[i..], state) {
                        return result;
                    }
                    val = val
                        .get_field(part)
                        .cloned()
                        .ok_or_else(|| format!("Field '{}' not found on PASSED", part))?;
                    i += 1;
                }
                Ok(val)
            }
        }
    }

    /// Try to resolve field access on a LINK value.
    /// Returns Some(result) if the value is a LINK and the access is event/text/hovered.
    /// Returns None if the value is not a LINK (continue normal field access).
    fn try_link_field_access(
        &self,
        val: &Value,
        remaining_parts: &[crate::parser::StrSlice],
        state: &ProgramState,
    ) -> Option<Result<Value, String>> {
        let link_path = match val {
            Value::Tagged { tag, fields } if tag.as_ref() == "LINK" => {
                fields.get("__path__" as &str)
                    .and_then(|v| v.as_text())
                    .map(|s| s.to_string())
            }
            Value::Tag(t) if t.as_ref() == "LINK" => {
                // LINK without path — can't resolve events
                None
            }
            _ => return None,
        };

        let link_path = link_path?;

        if remaining_parts.is_empty() {
            return None;
        }

        let first = remaining_parts[0].as_str();

        if first == "event" {
            // Construct event key from link_path + remaining parts
            let event_subpath: Vec<&str> = remaining_parts.iter().map(|p| p.as_str()).collect();
            let full_key = format!("{}.{}", link_path, event_subpath.join("."));

            // Try exact match first
            if let Some(event_val) = state.fired_events.get(&full_key) {
                return Some(Ok(event_val.clone()));
            }

            // Try partial matches (e.g., "path.event.key_down" returns object, then ".key" on result)
            for end in (1..remaining_parts.len()).rev() {
                let sub: Vec<&str> = remaining_parts[..=end].iter().map(|p| p.as_str()).collect();
                let key = format!("{}.{}", link_path, sub.join("."));
                if let Some(event_val) = state.fired_events.get(&key) {
                    let mut result = event_val.clone();
                    for part_idx in (end + 1)..remaining_parts.len() {
                        result = match result.get_field(remaining_parts[part_idx].as_str()) {
                            Some(v) => v.clone(),
                            None => return Some(Err(format!(
                                "Field '{}' not found on event value",
                                remaining_parts[part_idx].as_str()
                            ))),
                        };
                    }
                    return Some(Ok(result));
                }
            }

            // No event fired — check persistent state as fallback.
            // e.g., element.event.change.text should return current text from text_inputs
            // even when no change event is currently firing (LATEST semantics).
            let parts: Vec<&str> = remaining_parts.iter().map(|p| p.as_str()).collect();
            if parts.ends_with(&["change", "text"]) || parts.ends_with(&["change"]) {
                if let Some(text) = state.text_inputs.get(&link_path) {
                    if parts.ends_with(&["change", "text"]) {
                        return Some(Ok(Value::text(text.as_str())));
                    } else {
                        return Some(Ok(Value::object([("text", Value::text(text.as_str()))])));
                    }
                }
            }
            // No persistent state either — return SKIP
            Some(Ok(Value::tag("SKIP")))
        } else if first == "text" {
            // Text input current value
            if let Some(text) = state.text_inputs.get(&link_path) {
                Some(Ok(Value::text(text.as_str())))
            } else {
                Some(Ok(Value::text("")))
            }
        } else if first == "hovered" {
            // Hovered state
            let hovered = state.hovered.get(&link_path).copied().unwrap_or(false);
            Some(Ok(Value::bool(hovered)))
        } else {
            // Not a known LINK field — fall through to normal access
            None
        }
    }

    fn resolve_alias(
        &self,
        name: &str,
        scope: &[(&str, Value)],
        state: &ProgramState,
        passed: Option<&Value>,
    ) -> Result<Value, String> {
        // Check local scope first
        for (n, v) in scope.iter().rev() {
            if *n == name {
                return Ok(v.clone());
            }
        }

        // Check HOLD states
        if let Some(val) = state.holds.get(name) {
            return Ok(val.clone());
        }

        // Check LIST states
        if let Some(items) = state.lists.get(name) {
            return Ok(self.list_to_value(items));
        }

        // Check top-level variables
        if let Some(expr) = self.get_var_expr(name) {
            let prev = self.current_eval_var.borrow().clone();
            *self.current_eval_var.borrow_mut() = Some(name.to_string());
            let prev_prefix = self.current_link_prefix.borrow().clone();
            *self.current_link_prefix.borrow_mut() = name.to_string();
            let result = self.eval(expr, scope, state, passed);
            *self.current_link_prefix.borrow_mut() = prev_prefix;
            *self.current_eval_var.borrow_mut() = prev;
            return result;
        }

        // Sibling resolution: if we're evaluating a dotted variable like "store.items",
        // try resolving bare names as siblings (e.g., "text_to_add" → "store.text_to_add")
        // Clone to release the borrow before any nested borrow_mut calls.
        let current_var_snapshot = self.current_eval_var.borrow().clone();
        if let Some(ref current_var) = current_var_snapshot {
            if let Some(dot_pos) = current_var.rfind('.') {
                let parent = &current_var[..dot_pos + 1];
                let sibling_name = format!("{}{}", parent, name);
                // Check HOLD states with sibling name
                if let Some(val) = state.holds.get(&sibling_name) {
                    return Ok(val.clone());
                }
                // Check LIST states with sibling name
                if let Some(items) = state.lists.get(&sibling_name) {
                    return Ok(self.list_to_value(items));
                }
                // Check top-level variables with sibling name
                if let Some(expr) = self.get_var_expr(&sibling_name) {
                    let prev = self.current_eval_var.borrow().clone();
                    *self.current_eval_var.borrow_mut() = Some(sibling_name.clone());
                    let prev_prefix = self.current_link_prefix.borrow().clone();
                    *self.current_link_prefix.borrow_mut() = sibling_name.clone();
                    let result = self.eval(expr, scope, state, passed);
                    *self.current_link_prefix.borrow_mut() = prev_prefix;
                    *self.current_eval_var.borrow_mut() = prev;
                    return result;
                }
                // Check fired events with sibling name
                if let Some(val) = state.fired_events.get(&sibling_name) {
                    return Ok(val.clone());
                }
            }
        }

        // Check fired events (for event paths)
        if let Some(val) = state.fired_events.get(name) {
            return Ok(val.clone());
        }

        // Special names
        match name {
            "True" => return Ok(Value::bool(true)),
            "False" => return Ok(Value::bool(false)),
            "NoElement" => return Ok(Value::tag("NoElement")),
            "NoOutline" => return Ok(Value::tag("NoOutline")),
            "element" => {
                // `element` refers to the current element's state (events, hovered, etc.)
                // Return a LINK-like value with the current link prefix so that
                // element.event.change.text, element.hovered, etc. resolve via try_link_field_access
                let path = self.current_link_prefix.borrow().clone();
                if !path.is_empty() {
                    let mut fields = BTreeMap::new();
                    fields.insert(Arc::from("__path__"), Value::text(path.as_str()));
                    return Ok(Value::Tagged {
                        tag: Arc::from("LINK"),
                        fields: Arc::new(fields),
                    });
                }
                return Ok(self.build_element_value(scope, state));
            }
            "LINK" => {
                // LINK literal — create a LINK value with current path
                let path = self.current_link_prefix.borrow().clone();
                if path.is_empty() {
                    return Ok(Value::tag("LINK"));
                } else {
                    let mut fields = BTreeMap::new();
                    fields.insert(Arc::from("__path__"), Value::text(path.as_str()));
                    return Ok(Value::Tagged {
                        tag: Arc::from("LINK"),
                        fields: Arc::new(fields),
                    });
                }
            }
            _ => {}
        }

        Err(format!("Variable '{}' not found", name))
    }

    fn build_element_value(&self, _scope: &[(&str, Value)], state: &ProgramState) -> Value {
        // Build element value with hovered state and fired events
        let mut fields = BTreeMap::new();

        // Add hovered state - check all hovered entries
        // The specific element's hover state depends on context
        // For now, return False
        fields.insert(Arc::from("hovered"), Value::bool(false));

        Value::Object(Arc::new(fields))
    }

    fn get_var_expr(&self, name: &str) -> Option<&Spanned<Expression>> {
        // Direct match first
        if let Some((_, e)) = self.variables.iter().find(|(n, _)| n == name) {
            return Some(e);
        }
        // For dotted names like "store.elements", navigate into Object expressions
        if let Some(dot_pos) = name.find('.') {
            let parent = &name[..dot_pos];
            let child = &name[dot_pos + 1..];
            if let Some(parent_expr) = self.get_var_expr(parent) {
                return Self::find_field_in_expr(parent_expr, child);
            }
        }
        None
    }

    /// Navigate into an Object expression to find a nested field by dotted path.
    fn find_field_in_expr<'a>(
        expr: &'a Spanned<Expression>,
        field_path: &str,
    ) -> Option<&'a Spanned<Expression>> {
        if let Expression::Object(obj) = &expr.node {
            let (field_name, rest) = match field_path.find('.') {
                Some(pos) => (&field_path[..pos], Some(&field_path[pos + 1..])),
                None => (field_path, None),
            };
            for var in &obj.variables {
                if var.node.name.as_str() == field_name {
                    return match rest {
                        Some(rest) => Self::find_field_in_expr(&var.node.value, rest),
                        None => Some(&var.node.value),
                    };
                }
            }
        }
        None
    }

    /// Embed a per-item prefix in a list item Value as a hidden field.
    /// Used by HOLD sync to identify which item to update.
    fn embed_item_prefix(val: Value, prefix: &str) -> Value {
        if let Value::Object(fields) = &val {
            let mut new_fields = (**fields).clone();
            new_fields.insert(Arc::from("__item_prefix__"), Value::text(prefix));
            Value::Object(Arc::new(new_fields))
        } else {
            val
        }
    }

    fn list_to_value(&self, items: &[Value]) -> Value {
        let mut fields = BTreeMap::new();
        for (i, item) in items.iter().enumerate() {
            fields.insert(Arc::from(format!("{:04}", i)), item.clone());
        }
        Value::Tagged {
            tag: Arc::from("List"),
            fields: Arc::new(fields),
        }
    }

    // -----------------------------------------------------------------------
    // Pipe evaluation
    // -----------------------------------------------------------------------

    fn eval_pipe(
        &self,
        from: &Spanned<Expression>,
        to: &Spanned<Expression>,
        scope: &[(&str, Value)],
        state: &ProgramState,
        passed: Option<&Value>,
    ) -> Result<Value, String> {
        match &to.node {
            Expression::FunctionCall { path, arguments } => {
                let from_val = self.eval(from, scope, state, passed)?;
                // Don't skip accumulators (Math/sum, Document/new, Stream/skip, Log/info)
                let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                let is_accumulator = matches!(
                    path_strs.as_slice(),
                    ["Math", "sum"] | ["Document", "new"] | ["Stream", "skip"] | ["Log", "info"]
                );
                if self.is_skip(&from_val) && !is_accumulator {
                    return Ok(from_val);
                }
                self.eval_function_call(path, arguments, scope, state, passed, Some(&from_val))
            }

            Expression::Hold { state_param, body } => {
                // `initial |> HOLD state { body }`
                // Check for static loop pattern (Stream/pulses) first — these are
                // computed once and don't use state tracking (e.g., fibonacci).
                if self.extract_hold_loop_info(body, scope, state, passed).is_ok() {
                    return self.eval_hold_static(from, state_param.as_str(), body, scope, state, passed);
                }

                // Determine hold key: use current_link_prefix for per-item scoping
                let prefix = self.current_link_prefix.borrow().clone();
                let hold_name = if !prefix.is_empty() {
                    Some(prefix.clone())
                } else {
                    self.find_hold_name_in_scope(scope)
                        .or_else(|| self.current_eval_var.borrow().clone())
                };
                if let Some(ref hold_name) = hold_name {
                    // Always register this HOLD for future update_state processing,
                    // even if the value is already in state (e.g., from persistence).
                    // Per-item holds (inside list items) are the main case for discovery.
                    let is_known_var = self.variables.iter().any(|(n, _)| n == hold_name);
                    if !is_known_var {
                        let already = self.discovered_holds.borrow()
                            .iter().any(|h| h.name == *hold_name);
                        if !already {
                            let scope_bindings: Vec<(String, Value)> = scope
                                .iter()
                                .map(|(n, v)| (n.to_string(), v.clone()))
                                .collect();
                            self.discovered_holds.borrow_mut().push(DiscoveredHold {
                                name: hold_name.clone(),
                                state_param: state_param.to_string(),
                                body: (**body).clone(),
                                initial_expr: from.clone(),
                                scope_bindings,
                            });
                        }
                    }

                    // Look up current state (already initialized, e.g., from persistence)
                    if let Some(current) = state.holds.get(hold_name.as_str()) {
                        return Ok(current.clone());
                    }
                    // Initialize hold state if not yet present
                    let initial = self.eval(from, scope, state, passed)?;
                    return Ok(initial);
                }
                // Fallback: try static evaluation
                self.eval_hold_static(from, state_param.as_str(), body, scope, state, passed)
            }

            Expression::Then { body } => {
                let from_val = self.eval(from, scope, state, passed)?;
                if self.is_skip(&from_val) {
                    return Ok(Value::tag("SKIP"));
                }
                self.eval(body, scope, state, passed)
            }

            Expression::When { arms } => {
                let from_val = self.eval(from, scope, state, passed)?;
                if self.is_skip(&from_val) {
                    return Ok(Value::tag("SKIP"));
                }
                self.eval_pattern_match(&from_val, arms, scope, state, passed)
            }

            Expression::While { arms } => {
                let from_val = self.eval(from, scope, state, passed)?;
                if self.is_skip(&from_val) {
                    return Ok(Value::tag("SKIP"));
                }
                self.eval_pattern_match(&from_val, arms, scope, state, passed)
            }

            Expression::FieldAccess { path } => {
                let mut val = self.eval(from, scope, state, passed)?;
                for field in path {
                    val = val
                        .get_field(field.as_str())
                        .cloned()
                        .ok_or_else(|| format!("Field '{}' not found", field.as_str()))?;
                }
                Ok(val)
            }

            Expression::LinkSetter { alias } => {
                // `element |> LINK { alias }` — inject link path into the element value
                // Resolve the alias to get the LINK value which carries the actual per-item path.
                let alias_val = self.eval_alias(&alias.node, scope, state, passed).ok();
                let resolved_path = alias_val.as_ref().and_then(|v| {
                    if let Value::Tagged { tag, fields } = v {
                        if tag.as_ref() == "LINK" {
                            return fields.get("__path__" as &str)
                                .and_then(|p| p.as_text())
                                .map(|s| s.to_string());
                        }
                    }
                    None
                });
                let alias_path = resolved_path.unwrap_or_else(|| {
                    match &alias.node {
                        Alias::WithoutPassed { parts, .. } => {
                            parts.iter().map(|p| p.as_str()).collect::<Vec<_>>().join(".")
                        }
                        Alias::WithPassed { extra_parts, .. } => {
                            extra_parts.iter().map(|p| p.as_str()).collect::<Vec<_>>().join(".")
                        }
                    }
                });
                // Set link prefix so LINKs inside the from expression get the correct path
                let old_prefix = self.current_link_prefix.borrow().clone();
                *self.current_link_prefix.borrow_mut() = alias_path.clone();
                let element = self.eval(from, scope, state, passed)?;
                *self.current_link_prefix.borrow_mut() = old_prefix;
                // Add link_path to the element's fields if it's a Tagged value
                if let Value::Tagged { tag, fields } = &element {
                    let mut new_fields = (**fields).clone();
                    new_fields.insert(Arc::from("press_link"), Value::text(alias_path.as_str()));
                    new_fields.insert(Arc::from("__link_path__"), Value::text(alias_path.as_str()));
                    Ok(Value::Tagged {
                        tag: tag.clone(),
                        fields: Arc::new(new_fields),
                    })
                } else {
                    Ok(element)
                }
            }

            _ => {
                // Generic pipe: evaluate both sides
                let _from_val = self.eval(from, scope, state, passed)?;
                self.eval(to, scope, state, passed)
            }
        }
    }

    fn find_hold_name_in_scope(&self, scope: &[(&str, Value)]) -> Option<String> {
        // Check if we're evaluating within a known variable context
        // This is a heuristic — the hold name is typically the variable that contains the HOLD
        None
    }

    /// Evaluate HOLD in static context (e.g., fibonacci loop)
    fn eval_hold_static(
        &self,
        initial_expr: &Spanned<Expression>,
        state_param: &str,
        body: &Spanned<Expression>,
        scope: &[(&str, Value)],
        state: &ProgramState,
        passed: Option<&Value>,
    ) -> Result<Value, String> {
        let initial = self.eval(initial_expr, scope, state, passed)?;

        // Try to extract loop count from body (Stream/pulses pattern)
        match self.extract_hold_loop_info(body, scope, state, passed) {
            Ok((count, transform_body)) => {
                // Run the loop
                let mut current = initial;
                for _ in 0..count {
                    let name_owned: String = state_param.to_string();
                    let name_leaked: &str =
                        unsafe { &*(name_owned.as_str() as *const str) };
                    std::mem::forget(name_owned);
                    let mut loop_scope = scope.to_vec();
                    loop_scope.push((name_leaked, current.clone()));
                    current = self.eval(transform_body, &loop_scope, state, passed)?;
                }
                // Mark that a static HOLD loop completed — Stream/skip should pass through
                self.static_hold_completed.set(true);
                Ok(current)
            }
            Err(_) => {
                // Not a loop pattern — just return initial value
                Ok(initial)
            }
        }
    }

    fn extract_hold_loop_info<'a>(
        &self,
        body: &'a Spanned<Expression>,
        scope: &[(&str, Value)],
        state: &ProgramState,
        passed: Option<&Value>,
    ) -> Result<(usize, &'a Spanned<Expression>), String> {
        match &body.node {
            Expression::Pipe { from, to } => match &to.node {
                Expression::Then { body: then_body } => {
                    let count = self.eval_stream_pulses_count(from, scope, state, passed)?;
                    Ok((count, then_body))
                }
                _ => Err("Expected THEN".to_string()),
            },
            _ => Err("Expected pipe".to_string()),
        }
    }

    fn eval_stream_pulses_count(
        &self,
        expr: &Spanned<Expression>,
        scope: &[(&str, Value)],
        state: &ProgramState,
        passed: Option<&Value>,
    ) -> Result<usize, String> {
        match &expr.node {
            Expression::Pipe { from, to } => {
                if let Expression::FunctionCall { path, .. } = &to.node {
                    let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                    if path_strs.as_slice() == &["Stream", "pulses"] {
                        let count_val = self.eval(from, scope, state, passed)?;
                        let count = count_val
                            .as_number()
                            .ok_or("Stream/pulses count must be a number")?;
                        return Ok(count as usize);
                    }
                }
                Err("Expected Stream/pulses".to_string())
            }
            _ => Err("Expected pipe to Stream/pulses".to_string()),
        }
    }

    // -----------------------------------------------------------------------
    // LATEST evaluation
    // -----------------------------------------------------------------------

    fn eval_latest(
        &self,
        inputs: &[Spanned<Expression>],
        scope: &[(&str, Value)],
        state: &ProgramState,
        passed: Option<&Value>,
    ) -> Result<Value, String> {
        // LATEST merges multiple inputs, using the most recently changed one.
        // In our interpreter: evaluate all inputs, use the first non-SKIP one
        // that corresponds to a fired event. If none fired, use the first
        // non-SKIP static value.
        let mut last_event_value = None;
        let mut first_static_value = None;

        for input in inputs {
            match self.eval(input, scope, state, passed) {
                Ok(val) => {
                    if self.is_skip(&val) {
                        continue;
                    }
                    // Check if this input involves a fired event
                    if self.expr_references_fired_event(input, state) {
                        last_event_value = Some(val);
                    } else if first_static_value.is_none() {
                        first_static_value = Some(val);
                    }
                }
                Err(_) => continue,
            }
        }

        // Prefer event value, fall back to static value, fall back to SKIP
        Ok(last_event_value
            .or(first_static_value)
            .unwrap_or_else(|| Value::tag("SKIP")))
    }

    fn expr_references_fired_event(
        &self,
        expr: &Spanned<Expression>,
        state: &ProgramState,
    ) -> bool {
        match &expr.node {
            Expression::Alias(Alias::WithoutPassed { parts, .. }) => {
                let full_path = parts
                    .iter()
                    .map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                if state.fired_events.contains_key(&full_path) {
                    return true;
                }
                // Handle `element.event.*` — resolve with current link prefix
                if parts.first().map(|p| p.as_str()) == Some("element") {
                    let prefix = self.current_link_prefix.borrow().clone();
                    if !prefix.is_empty() && parts.len() > 1 {
                        let suffix: Vec<&str> = parts[1..].iter().map(|p| p.as_str()).collect();
                        let resolved = format!("{}.{}", prefix, suffix.join("."));
                        if state.fired_events.contains_key(&resolved) {
                            return true;
                        }
                    }
                }
                // Also check with sibling resolution
                let current_var = self.current_eval_var.borrow().clone();
                if let Some(ref cv) = current_var {
                    if let Some(dot_pos) = cv.rfind('.') {
                        let parent = &cv[..dot_pos + 1];
                        let sibling = format!("{}{}", parent, full_path);
                        if state.fired_events.contains_key(&sibling) {
                            return true;
                        }
                    }
                }
                false
            }
            Expression::Alias(Alias::WithPassed { extra_parts, .. }) => {
                let full_path = extra_parts
                    .iter()
                    .map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                // Check if the PASSED alias path references a fired event
                state.fired_events.keys().any(|k| k.contains(&full_path))
            }
            Expression::Pipe { from, to } => {
                self.expr_references_fired_event(from, state)
                    || self.expr_references_fired_event(to, state)
            }
            Expression::Then { body } => true, // THEN always references events
            _ => false,
        }
    }

    // -----------------------------------------------------------------------
    // Pattern matching (WHEN/WHILE)
    // -----------------------------------------------------------------------

    fn eval_pattern_match(
        &self,
        input_value: &Value,
        arms: &[Arm],
        scope: &[(&str, Value)],
        state: &ProgramState,
        passed: Option<&Value>,
    ) -> Result<Value, String> {
        for arm in arms {
            let mut bindings = Vec::new();
            if self.match_pattern(input_value, &arm.pattern, &mut bindings) {
                let mut new_scope = scope.to_vec();
                for (name, val) in bindings {
                    let name_owned: String = name.to_string();
                    let name_leaked: &str =
                        unsafe { &*(name_owned.as_str() as *const str) };
                    std::mem::forget(name_owned);
                    new_scope.push((name_leaked, val));
                }
                return self.eval(&arm.body, &new_scope, state, passed);
            }
        }
        // No matching arm — return SKIP
        Ok(Value::tag("SKIP"))
    }

    fn match_pattern(
        &self,
        value: &Value,
        pattern: &Pattern,
        bindings: &mut Vec<(String, Value)>,
    ) -> bool {
        match pattern {
            Pattern::Literal(lit) => {
                let lit_val = Self::eval_literal(lit);
                self.values_match(value, &lit_val)
            }
            Pattern::Alias { name } => {
                bindings.push((name.as_str().to_string(), value.clone()));
                true
            }
            Pattern::WildCard => true,
            Pattern::Object { variables } => {
                for var in variables {
                    let name = var.name.as_str();
                    if let Some(field_val) = value.get_field(name) {
                        if let Some(ref sub_pattern) = var.value {
                            if !self.match_pattern(field_val, sub_pattern, bindings) {
                                return false;
                            }
                        } else {
                            bindings.push((name.to_string(), field_val.clone()));
                        }
                    } else {
                        return false;
                    }
                }
                true
            }
            Pattern::TaggedObject { tag, variables } => {
                if let Some(val_tag) = value.get_tag() {
                    if val_tag == tag.as_str() {
                        for var in variables {
                            let name = var.name.as_str();
                            if let Some(field_val) = value.get_field(name) {
                                if let Some(ref sub_pattern) = var.value {
                                    if !self.match_pattern(field_val, sub_pattern, bindings) {
                                        return false;
                                    }
                                } else {
                                    bindings.push((name.to_string(), field_val.clone()));
                                }
                            } else {
                                return false;
                            }
                        }
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
            Pattern::List { items: _ } => {
                // TODO: list pattern matching
                false
            }
            Pattern::Map { entries: _ } => {
                // TODO: map pattern matching
                false
            }
        }
    }

    fn values_match(&self, a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Number(n1), Value::Number(n2)) => n1 == n2,
            (Value::Text(s1), Value::Text(s2)) => s1 == s2,
            (Value::Tag(t1), Value::Tag(t2)) => t1 == t2,
            (Value::Bool(b1), Value::Bool(b2)) => b1 == b2,
            // Tag "True" matches Bool(true), etc.
            (Value::Tag(t), Value::Bool(b)) | (Value::Bool(b), Value::Tag(t)) => {
                (*b && t.as_ref() == "True") || (!*b && t.as_ref() == "False")
            }
            // Number 0 matches literal 0, etc.
            _ => a == b,
        }
    }

    // -----------------------------------------------------------------------
    // Function calls
    // -----------------------------------------------------------------------

    fn eval_function_call(
        &self,
        path: &[crate::parser::StrSlice],
        arguments: &[Spanned<Argument>],
        scope: &[(&str, Value)],
        state: &ProgramState,
        passed: Option<&Value>,
        pipe_input: Option<&Value>,
    ) -> Result<Value, String> {
        let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();

        match path_strs.as_slice() {
            ["Document", "new"] => {
                let mut fields = BTreeMap::new();
                for arg in arguments {
                    let name = arg.node.name.as_str();
                    if let Some(ref val_expr) = arg.node.value {
                        let val = self.eval(val_expr, scope, state, passed)?;
                        fields.insert(Arc::from(name), val);
                    }
                }
                if fields.is_empty() {
                    if let Some(input) = pipe_input {
                        if !self.is_skip(input) {
                            fields.insert(Arc::from("root"), input.clone());
                        }
                    }
                }
                Ok(Value::Tagged {
                    tag: Arc::from("DocumentNew"),
                    fields: Arc::new(fields),
                })
            }

            ["Element", elem_type] => {
                self.eval_element(elem_type, arguments, scope, state, passed)
            }

            ["Math", "sum"] => {
                // Math/sum — return accumulated sum from state
                let key = self.current_eval_var.borrow().clone().unwrap_or_default();
                if let Some(&sum) = state.sum_accumulators.get(&key) {
                    Ok(Value::number(sum))
                } else {
                    // No accumulator yet — no values have been summed
                    Ok(Value::tag("SKIP"))
                }
            }

            ["Stream", "pulses"] => {
                // Static: return the count
                if let Some(input) = pipe_input {
                    Ok(input.clone())
                } else {
                    Ok(Value::number(0.0))
                }
            }

            ["Stream", "skip"] => {
                // If a static HOLD loop just completed, pass through the final result
                if self.static_hold_completed.get() {
                    self.static_hold_completed.set(false);
                    if let Some(input) = pipe_input {
                        return Ok(input.clone());
                    }
                }
                // Stream/skip(count: N) — skip first N values
                if let Some(input) = pipe_input {
                    let skip_count = arguments
                        .iter()
                        .find(|a| a.node.name.as_str() == "count")
                        .and_then(|a| a.node.value.as_ref())
                        .and_then(|e| self.eval(e, scope, state, passed).ok())
                        .and_then(|v| v.as_number())
                        .map(|n| n as usize)
                        .unwrap_or(1);
                    let key = format!("__skip__");
                    let seen = state.skip_counts.get(&key).copied().unwrap_or(0);
                    if seen < skip_count {
                        Ok(Value::tag("SKIP"))
                    } else {
                        Ok(input.clone())
                    }
                } else {
                    Ok(Value::Unit)
                }
            }

            ["Log", "info"] => {
                // Passthrough
                if let Some(input) = pipe_input {
                    Ok(input.clone())
                } else {
                    Ok(Value::Unit)
                }
            }

            ["Router", "route"] => {
                Ok(Value::text(state.router_path.as_str()))
            }

            ["Router", "go_to"] => {
                // Side effect: navigate to URL
                if let Some(input) = pipe_input {
                    if let Some(path) = input.as_text() {
                        // The actual navigation happens via the event system
                        // Return the path as confirmation
                        return Ok(input.clone());
                    }
                }
                Ok(Value::Unit)
            }

            ["Timer", "interval"] => {
                // Timer input — check if this variable's timer fired
                let var = self.current_eval_var.borrow().clone().unwrap_or_default();
                let timer_key = format!("__timer__{}", var);
                if state.fired_events.contains_key(&timer_key) {
                    Ok(Value::Unit)
                } else {
                    Ok(Value::tag("SKIP"))
                }
            }

            ["Text", "trim"] => {
                if let Some(input) = pipe_input {
                    if let Some(s) = input.as_text() {
                        return Ok(Value::text(s.trim()));
                    }
                }
                Ok(Value::text(""))
            }

            ["Text", "is_not_empty"] => {
                if let Some(input) = pipe_input {
                    if let Some(s) = input.as_text() {
                        return Ok(Value::bool(!s.is_empty()));
                    }
                }
                Ok(Value::bool(false))
            }

            ["Text", "empty"] => Ok(Value::text("")),

            ["Text", "space"] => Ok(Value::text(" ")),

            ["Bool", "not"] => {
                if let Some(input) = pipe_input {
                    if let Some(b) = input.as_bool() {
                        return Ok(Value::bool(!b));
                    }
                }
                Ok(Value::bool(false))
            }

            ["Bool", "or"] => {
                if let Some(input) = pipe_input {
                    if let Some(b1) = input.as_bool() {
                        let that = arguments
                            .iter()
                            .find(|a| a.node.name.as_str() == "that")
                            .and_then(|a| a.node.value.as_ref())
                            .and_then(|e| self.eval(e, scope, state, passed).ok())
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        return Ok(Value::bool(b1 || that));
                    }
                }
                Ok(Value::bool(false))
            }

            ["List", "count"] => {
                if let Some(input) = pipe_input {
                    if let Value::Tagged { tag, fields } = input {
                        if tag.as_ref() == "List" {
                            return Ok(Value::number(fields.len() as f64));
                        }
                    }
                }
                Ok(Value::number(0.0))
            }

            ["List", "map"] => {
                if let Some(input) = pipe_input {
                    if let Value::Tagged { tag, fields } = input {
                        if tag.as_ref() == "List" {
                            let item_var = arguments
                                .iter()
                                .find(|a| a.node.value.is_none())
                                .map(|a| a.node.name.as_str())
                                .unwrap_or("item");
                            let new_expr = arguments
                                .iter()
                                .find(|a| a.node.name.as_str() == "new")
                                .and_then(|a| a.node.value.as_ref());

                            if let Some(new_expr) = new_expr {
                                let mut new_fields = BTreeMap::new();
                                for (key, val) in fields.iter() {
                                    let name_owned: String = item_var.to_string();
                                    let name_leaked: &str =
                                        unsafe { &*(name_owned.as_str() as *const str) };
                                    std::mem::forget(name_owned);
                                    let mut item_scope = scope.to_vec();
                                    item_scope.push((name_leaked, val.clone()));

                                    // Scope the link prefix per list item so each item's
                                    // LINKs get unique paths (e.g., "todos.0000.todo_elements.todo_checkbox")
                                    // Use __item_prefix__ from the item value if available, since
                                    // list_to_value re-keys items sequentially but HOLDs use
                                    // the original prefix (e.g., "store.todos.0005" not "store.todos.0000")
                                    let old_prefix = self.current_link_prefix.borrow().clone();
                                    let item_prefix = val.get_field("__item_prefix__")
                                        .and_then(|v| v.as_text().map(|s| s.to_string()))
                                        .unwrap_or_else(|| {
                                            if old_prefix.is_empty() {
                                                key.to_string()
                                            } else {
                                                format!("{}.{}", old_prefix, key)
                                            }
                                        });
                                    *self.current_link_prefix.borrow_mut() = item_prefix;

                                    let mapped = self.eval(new_expr, &item_scope, state, passed)?;

                                    *self.current_link_prefix.borrow_mut() = old_prefix;

                                    new_fields.insert(key.clone(), mapped);
                                }
                                return Ok(Value::Tagged {
                                    tag: Arc::from("List"),
                                    fields: Arc::new(new_fields),
                                });
                            }
                        }
                    }
                }
                Ok(pipe_input.cloned().unwrap_or(Value::Unit))
            }

            ["List", "retain"] => {
                if let Some(input) = pipe_input {
                    if let Value::Tagged { tag, fields } = input {
                        if tag.as_ref() == "List" {
                            let item_var = arguments
                                .iter()
                                .find(|a| a.node.value.is_none())
                                .map(|a| a.node.name.as_str())
                                .unwrap_or("item");
                            let if_expr = arguments
                                .iter()
                                .find(|a| a.node.name.as_str() == "if")
                                .and_then(|a| a.node.value.as_ref());

                            if let Some(if_expr) = if_expr {
                                let mut new_fields = BTreeMap::new();
                                for (key, val) in fields.iter() {
                                    let name_owned: String = item_var.to_string();
                                    let name_leaked: &str =
                                        unsafe { &*(name_owned.as_str() as *const str) };
                                    std::mem::forget(name_owned);
                                    let mut item_scope = scope.to_vec();
                                    item_scope.push((name_leaked, val.clone()));
                                    let keep = self.eval(if_expr, &item_scope, state, passed)?;
                                    if keep.as_bool().unwrap_or(false) {
                                        new_fields.insert(key.clone(), val.clone());
                                    }
                                }
                                return Ok(Value::Tagged {
                                    tag: Arc::from("List"),
                                    fields: Arc::new(new_fields),
                                });
                            }
                        }
                    }
                }
                Ok(pipe_input.cloned().unwrap_or(Value::Unit))
            }

            ["List", "is_empty"] => {
                if let Some(input) = pipe_input {
                    if let Value::Tagged { tag, fields } = input {
                        if tag.as_ref() == "List" {
                            return Ok(Value::bool(fields.is_empty()));
                        }
                    }
                }
                Ok(Value::bool(true))
            }

            ["List", "append"] | ["List", "clear"] | ["List", "remove"] => {
                // These are handled in update_list_state
                // In document evaluation, just return the current list value
                if let Some(input) = pipe_input {
                    Ok(input.clone())
                } else {
                    Ok(Value::Unit)
                }
            }

            [fn_name] => {
                // User-defined function call
                self.eval_user_function(fn_name, arguments, scope, state, passed, pipe_input)
            }

            _ => Err(format!("Unknown function: {}", path_strs.join("/"))),
        }
    }

    fn eval_element(
        &self,
        elem_type: &str,
        arguments: &[Spanned<Argument>],
        scope: &[(&str, Value)],
        state: &ProgramState,
        passed: Option<&Value>,
    ) -> Result<Value, String> {
        let tag = format!("Element{}", to_pascal_case(elem_type));
        let mut fields = BTreeMap::new();
        for arg in arguments {
            let name = arg.node.name.as_str().to_string();
            if let Some(ref val_expr) = arg.node.value {
                let val = self.eval(val_expr, scope, state, passed)?;
                fields.insert(Arc::from(name.as_str()), val);
            }
        }
        Ok(Value::Tagged {
            tag: Arc::from(tag.as_str()),
            fields: Arc::new(fields),
        })
    }

    fn eval_user_function(
        &self,
        fn_name: &str,
        arguments: &[Spanned<Argument>],
        scope: &[(&str, Value)],
        state: &ProgramState,
        passed: Option<&Value>,
        pipe_input: Option<&Value>,
    ) -> Result<Value, String> {
        let func = self
            .functions
            .iter()
            .find(|(name, _, _)| name == fn_name)
            .ok_or_else(|| format!("Function '{}' not found", fn_name))?
            .clone();

        let (_, params, body) = func;

        // Build function scope
        let mut fn_scope: Vec<(&str, Value)> = Vec::new();

        // Check for PASS argument
        let mut new_passed = passed.cloned();
        for arg in arguments {
            let arg_name = arg.node.name.as_str();
            if arg_name == "PASS" {
                if let Some(ref val_expr) = arg.node.value {
                    let val = self.eval(val_expr, scope, state, passed)?;
                    new_passed = Some(val);
                }
                continue;
            }
            if let Some(ref val_expr) = arg.node.value {
                let val = self.eval(val_expr, scope, state, passed)?;
                let name_owned: String = arg_name.to_string();
                let name_leaked: &str = unsafe { &*(name_owned.as_str() as *const str) };
                std::mem::forget(name_owned);
                fn_scope.push((name_leaked, val));
            }
        }

        // Bind positional params
        for (i, param_name) in params.iter().enumerate() {
            if i < arguments.len() && arguments[i].node.name.as_str() != "PASS" {
                // Already handled by named args above
            }
        }

        // Add pipe input as first positional param if applicable
        if let Some(input) = pipe_input {
            if !params.is_empty() {
                let name_owned: String = params[0].clone();
                let name_leaked: &str = unsafe { &*(name_owned.as_str() as *const str) };
                std::mem::forget(name_owned);
                fn_scope.push((name_leaked, input.clone()));
            }
        }

        self.eval(&body, &fn_scope, state, new_passed.as_ref())
    }

    // -----------------------------------------------------------------------
    // Arithmetic and comparison
    // -----------------------------------------------------------------------

    fn eval_arithmetic(
        &self,
        op: &ArithmeticOperator,
        scope: &[(&str, Value)],
        state: &ProgramState,
        passed: Option<&Value>,
    ) -> Result<Value, String> {
        match op {
            ArithmeticOperator::Add {
                operand_a,
                operand_b,
            } => {
                let a = self.eval(operand_a, scope, state, passed)?;
                let b = self.eval(operand_b, scope, state, passed)?;
                Ok(Value::number(
                    a.as_number().unwrap_or(0.0) + b.as_number().unwrap_or(0.0),
                ))
            }
            ArithmeticOperator::Subtract {
                operand_a,
                operand_b,
            } => {
                let a = self.eval(operand_a, scope, state, passed)?;
                let b = self.eval(operand_b, scope, state, passed)?;
                Ok(Value::number(
                    a.as_number().unwrap_or(0.0) - b.as_number().unwrap_or(0.0),
                ))
            }
            ArithmeticOperator::Multiply {
                operand_a,
                operand_b,
            } => {
                let a = self.eval(operand_a, scope, state, passed)?;
                let b = self.eval(operand_b, scope, state, passed)?;
                Ok(Value::number(
                    a.as_number().unwrap_or(0.0) * b.as_number().unwrap_or(0.0),
                ))
            }
            ArithmeticOperator::Divide {
                operand_a,
                operand_b,
            } => {
                let a = self.eval(operand_a, scope, state, passed)?;
                let b = self.eval(operand_b, scope, state, passed)?;
                let bv = b.as_number().unwrap_or(1.0);
                if bv == 0.0 {
                    Ok(Value::number(f64::NAN))
                } else {
                    Ok(Value::number(a.as_number().unwrap_or(0.0) / bv))
                }
            }
            ArithmeticOperator::Negate { operand } => {
                let v = self.eval(operand, scope, state, passed)?;
                Ok(Value::number(-v.as_number().unwrap_or(0.0)))
            }
        }
    }

    fn eval_comparator(
        &self,
        cmp: &static_expression::Comparator,
        scope: &[(&str, Value)],
        state: &ProgramState,
        passed: Option<&Value>,
    ) -> Result<Value, String> {
        use static_expression::Comparator;
        match cmp {
            Comparator::Equal {
                operand_a,
                operand_b,
            } => {
                let a = self.eval(operand_a, scope, state, passed)?;
                let b = self.eval(operand_b, scope, state, passed)?;
                Ok(Value::bool(self.values_match(&a, &b)))
            }
            Comparator::NotEqual {
                operand_a,
                operand_b,
            } => {
                let a = self.eval(operand_a, scope, state, passed)?;
                let b = self.eval(operand_b, scope, state, passed)?;
                Ok(Value::bool(!self.values_match(&a, &b)))
            }
            Comparator::Greater {
                operand_a,
                operand_b,
            } => {
                let a = self.eval(operand_a, scope, state, passed)?;
                let b = self.eval(operand_b, scope, state, passed)?;
                Ok(Value::bool(a > b))
            }
            Comparator::GreaterOrEqual {
                operand_a,
                operand_b,
            } => {
                let a = self.eval(operand_a, scope, state, passed)?;
                let b = self.eval(operand_b, scope, state, passed)?;
                Ok(Value::bool(a >= b))
            }
            Comparator::Less {
                operand_a,
                operand_b,
            } => {
                let a = self.eval(operand_a, scope, state, passed)?;
                let b = self.eval(operand_b, scope, state, passed)?;
                Ok(Value::bool(a < b))
            }
            Comparator::LessOrEqual {
                operand_a,
                operand_b,
            } => {
                let a = self.eval(operand_a, scope, state, passed)?;
                let b = self.eval(operand_b, scope, state, passed)?;
                Ok(Value::bool(a <= b))
            }
        }
    }

    // -----------------------------------------------------------------------
    // Utility
    // -----------------------------------------------------------------------

    fn is_skip(&self, value: &Value) -> bool {
        matches!(value, Value::Tag(t) if t.as_ref() == "SKIP")
    }
}

fn to_pascal_case(s: &str) -> String {
    s.split('_')
        .map(|word| {
            let mut c = word.chars();
            match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().to_string() + c.as_str(),
            }
        })
        .collect()
}

