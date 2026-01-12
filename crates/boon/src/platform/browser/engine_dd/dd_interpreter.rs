//! DD Interpreter - Parses Boon code and evaluates using Differential Dataflow.
//!
//! This module provides the entry point for running Boon code with the DD engine.
//! It uses the existing parser infrastructure and `BoonDdRuntime` for evaluation.
//!
//! # Architecture
//!
//! 1. Parse source code → AST
//! 2. Resolve references and persistence
//! 3. Convert to static expressions
//! 4. Evaluate with `BoonDdRuntime`
//! 5. Return `DdResult` with document and context
//!
//! # Current Limitations
//!
//! - Static evaluation only (no reactive LINK events yet)
//! - No timer support yet
//! - No persistence support yet
//!
//! These will be added in subsequent phases using DdWorker.

use chumsky::Parser as _;
use super::dd_evaluator::{BoonDdRuntime, reset_hold_counter};
use super::dd_value::DdValue;
use super::core::{DdWorker, DataflowConfig, HoldConfig, HoldId, LinkId, EventFilter, StateTransform};
use super::io::{EventInjector, set_global_dispatcher, clear_global_dispatcher, set_task_handle, clear_task_handle, set_output_listener_handle, clear_output_listener_handle, set_timer_handle, clear_timer_handle, clear_router_mappings, update_hold_state, update_hold_state_no_persist, load_persisted_hold_value, set_checkbox_toggle_holds, clear_hold_states_memory};
#[cfg(target_arch = "wasm32")]
use super::dd_bridge::clear_dd_text_input_value;
use zoon::{Task, StreamExt};
use crate::parser::{
    Input, SourceCode, Spanned, Token, lexer, parser, reset_expression_depth,
    resolve_persistence, resolve_references, span_at, static_expression,
};

/// Result of running DD reactive evaluation.
#[derive(Clone)]
pub struct DdResult {
    /// The document value if evaluation succeeded
    pub document: Option<DdValue>,
    /// Evaluation context with runtime information
    pub context: DdContext,
}

/// Context for DD evaluation containing runtime state.
#[derive(Clone, Default)]
pub struct DdContext {
    /// Active timers (empty for static evaluation)
    timers: Vec<TimerInfo>,
    /// Whether there are sum accumulators
    has_accumulators: bool,
}

/// Information about an active timer.
#[derive(Clone)]
pub struct TimerInfo {
    /// Timer identifier
    pub id: String,
    /// Interval in milliseconds
    pub interval_ms: u64,
}

impl DdContext {
    /// Create a new empty context.
    pub fn new() -> Self {
        Self::default()
    }

    /// Get active timers.
    pub fn get_timers(&self) -> &[TimerInfo] {
        &self.timers
    }

    /// Check if there are sum accumulators.
    pub fn has_sum_accumulators(&self) -> bool {
        self.has_accumulators
    }

    /// Add a timer to the context.
    pub fn add_timer(&mut self, id: String, interval_ms: u64) {
        self.timers.push(TimerInfo { id, interval_ms });
    }

    /// Mark that sum accumulators are present.
    pub fn set_has_accumulators(&mut self, has: bool) {
        self.has_accumulators = has;
    }
}

/// Run Boon code with DD reactive evaluation and persistence.
///
/// # Arguments
///
/// * `filename` - The name of the file being run
/// * `source_code` - The Boon source code to evaluate
/// * `states_storage_key` - Optional localStorage key for persisted state
///
/// # Returns
///
/// `Some(DdResult)` if evaluation succeeded, `None` if parsing failed.
pub fn run_dd_reactive_with_persistence(
    filename: &str,
    source_code: &str,
    _states_storage_key: Option<&str>,
) -> Option<DdResult> {
    zoon::println!("[DD Interpreter] Parsing: {}", filename);

    // Clean up any existing components from previous runs
    // This ensures old timers/workers stop before new ones start
    clear_timer_handle();
    clear_output_listener_handle();
    clear_task_handle();
    clear_global_dispatcher();
    clear_router_mappings();
    clear_hold_states_memory();  // Prevent state contamination between examples
    #[cfg(target_arch = "wasm32")]
    clear_dd_text_input_value();  // Clear text input state
    reset_hold_counter();

    // Create SourceCode for the parser
    let source_code_arc = SourceCode::new(source_code.to_string());
    let source_str = source_code_arc.as_str();

    // Step 1: Lexer
    let (tokens, lex_errors) = lexer().parse(source_str).into_output_errors();
    if !lex_errors.is_empty() {
        zoon::eprintln!("[DD Interpreter] Lex errors:");
        for err in &lex_errors {
            zoon::eprintln!("  {:?}", err);
        }
        return None;
    }
    let Some(mut tokens) = tokens else {
        zoon::eprintln!("[DD Interpreter] Lexer produced no tokens");
        return None;
    };

    // Remove comments
    tokens.retain(|spanned_token| !matches!(spanned_token.node, Token::Comment(_)));

    // Step 2: Parser
    reset_expression_depth();
    let (ast, parse_errors) = parser()
        .parse(tokens.map(
            span_at(source_str.len()),
            |Spanned { node, span, persistence: _ }| (node, span),
        ))
        .into_output_errors();
    if !parse_errors.is_empty() {
        zoon::eprintln!("[DD Interpreter] Parse errors:");
        for err in &parse_errors {
            zoon::eprintln!("  {:?}", err);
        }
        return None;
    }
    let Some(ast) = ast else {
        zoon::eprintln!("[DD Interpreter] Parser produced no AST");
        return None;
    };

    // Step 3: Resolve references
    let ast = match resolve_references(ast) {
        Ok(ast) => ast,
        Err(errors) => {
            zoon::eprintln!("[DD Interpreter] Reference errors:");
            for err in &errors {
                zoon::eprintln!("  {:?}", err);
            }
            return None;
        }
    };

    // Step 4: Resolve persistence (with empty old AST for now)
    let (ast, _span_id_pairs) = match resolve_persistence(ast, None, "dd_span_ids") {
        Ok(result) => result,
        Err(errors) => {
            zoon::eprintln!("[DD Interpreter] Persistence errors:");
            for err in &errors {
                zoon::eprintln!("  {:?}", err);
            }
            return None;
        }
    };

    // Step 5: Convert to static expressions
    let static_ast = static_expression::convert_expressions(source_code_arc.clone(), ast);

    // Step 6: Evaluate with BoonDdRuntime
    let mut runtime = BoonDdRuntime::new();
    runtime.evaluate(&static_ast);

    // Get the document output
    let document = runtime.get_document().cloned();

    // Get the initial todos list from static evaluation (for todo_mvc)
    // In todo_mvc.bn, todos is inside store: [todos: LIST {...}]
    let static_todos = runtime.get_variable("store")
        .and_then(|store| store.get("todos"))
        .cloned()
        .or_else(|| runtime.get_variable("todos").cloned());
    // Get the initial items list from static evaluation (for shopping_list)
    let static_items = runtime.get_variable("items").cloned();

    zoon::println!("[DD Interpreter] Evaluation complete, has document: {}",
        document.is_some());
    zoon::println!("[DD Interpreter] static_todos = {:?}", static_todos);
    zoon::println!("[DD Interpreter] static_items = {:?}", static_items);

    // Step 7: Set up DdWorker for reactive updates
    // Detect which pattern the code uses and configure accordingly
    let timer_info = extract_timer_info(&document);
    let key_down_link = extract_text_input_key_down(&document);
    let button_press_link = extract_button_press_link(&document);
    let checkbox_toggles = extract_checkbox_toggles(&document);
    let editing_toggles = extract_editing_toggles(&document);
    let toggle_all_link = extract_toggle_all_link(&document);
    let clear_completed_link = extract_clear_completed_button_link(&document);

    let config = if let Some((ref hold_id, interval_ms)) = timer_info {
        // Timer-driven pattern: Duration |> Timer/interval() |> THEN |> Math/sum()
        // Timer values are NOT persisted - they're time-based, not user data
        // Start from 0 every time, don't pre-populate HOLD_STATES (so preview is empty until first tick)
        let initial_value = DdValue::int(0);
        zoon::println!("[DD Interpreter] Timer config: {} @ {}ms, initial {:?}", hold_id, interval_ms, initial_value);
        DataflowConfig::timer_counter(hold_id, initial_value, interval_ms)
    } else if !checkbox_toggles.is_empty() {
        // Checkbox toggle pattern (todo_mvc): checkbox.click |> THEN { state |> Bool/not() }
        // Each checkbox has its own HOLD for the completed state
        zoon::println!("[DD Interpreter] Checkbox toggle config: {} toggles", checkbox_toggles.len());
        let mut config = DataflowConfig::new();
        let mut checkbox_hold_ids = Vec::new();
        for toggle in &checkbox_toggles {
            // Initialize HOLD_STATES for reactive rendering
            update_hold_state_no_persist(&toggle.hold_id, DdValue::Bool(toggle.initial));
            checkbox_hold_ids.push(toggle.hold_id.clone());
            // Build triggered_by list: own checkbox click + toggle_all click (if present)
            let mut triggers = vec![LinkId::new(&toggle.link_id)];
            if let Some(ref toggle_all_id) = toggle_all_link {
                triggers.push(LinkId::new(toggle_all_id));
            }
            // Add BoolToggle HOLD config
            config.holds.push(HoldConfig {
                id: HoldId::new(&toggle.hold_id),
                initial: DdValue::Bool(toggle.initial),
                triggered_by: triggers,
                timer_interval_ms: 0,
                filter: EventFilter::Any,
                transform: StateTransform::BoolToggle,
                persist: true,
            });
        }
        // Register checkbox hold IDs for reactive "N items left" count
        set_checkbox_toggle_holds(checkbox_hold_ids);

        // Add editing toggle HOLDs (for double-click to edit in todo_mvc)
        for toggle in &editing_toggles {
            // Initialize editing HOLD to false (not editing initially)
            update_hold_state_no_persist(&toggle.hold_id, DdValue::Bool(false));

            // Add SetTrue HOLD triggered by double_click
            config.holds.push(HoldConfig {
                id: HoldId::new(&toggle.hold_id),
                initial: DdValue::Bool(false),
                triggered_by: vec![LinkId::new(&toggle.double_click_link)],
                timer_interval_ms: 0,
                filter: EventFilter::Any,
                transform: StateTransform::SetTrue,
                persist: false, // Don't persist editing state
            });

            // NOTE: Blur HoldConfig is intentionally NOT added here because:
            // When inner events (change, key_down, blur) share the same LinkRef (link_53),
            // EventFilter::Any on blur would trigger on change events too, immediately
            // exiting edit mode. This is a known limitation until the interpreter creates
            // unique LinkRefs per event type. For now, rely on Enter/Escape to exit editing.
            // See: https://github.com/anthropics/boon/issues/XXX (TODO: file issue)
            let _ = &toggle.blur_link; // silence unused warning

            // Add SetFalse HOLD triggered by key_down with Enter or Escape (if present)
            if let Some(ref key_down_link) = toggle.key_down_link {
                // For Enter key
                config.holds.push(HoldConfig {
                    id: HoldId::new(&toggle.hold_id),
                    initial: DdValue::Bool(false),
                    triggered_by: vec![LinkId::new(key_down_link)],
                    timer_interval_ms: 0,
                    filter: EventFilter::TextEquals("Enter".to_string()),
                    transform: StateTransform::SetFalse,
                    persist: false,
                });
                // For Escape key
                config.holds.push(HoldConfig {
                    id: HoldId::new(&toggle.hold_id),
                    initial: DdValue::Bool(false),
                    triggered_by: vec![LinkId::new(key_down_link)],
                    timer_interval_ms: 0,
                    filter: EventFilter::TextEquals("Escape".to_string()),
                    transform: StateTransform::SetFalse,
                    persist: false,
                });
            }

            zoon::println!("[DD Interpreter] Added editing toggle config for hold {}", toggle.hold_id);
        }

        // Also check for text input key_down pattern (todo_mvc has BOTH checkboxes AND add-todo input)
        if let Some(ref link_id) = key_down_link {
            // Use persisted value, or fall back to static evaluation, or empty list
            let initial_list = load_persisted_hold_value("todos")
                .unwrap_or_else(|| static_todos.clone().unwrap_or_else(|| DdValue::List(std::sync::Arc::new(Vec::new()))));
            zoon::println!("[DD Interpreter] todos initial_list: {:?}", initial_list);
            // Initialize HOLD_STATES for reactive rendering
            update_hold_state_no_persist("todos", initial_list.clone());
            // Initialize text_input_text HOLD for reactive text clearing
            update_hold_state_no_persist("text_input_text", DdValue::text(""));
            zoon::println!("[DD Interpreter] Also adding list-append for todos: link={}", link_id);
            // Add list-append HOLDs manually (can't use add_list_append_on_enter which returns new Self)
            config.holds.push(HoldConfig {
                id: HoldId::new("todos"),
                initial: initial_list.clone(),
                triggered_by: vec![LinkId::new(link_id)],
                timer_interval_ms: 0,
                filter: EventFilter::TextStartsWith("Enter:".to_string()),
                transform: StateTransform::ListAppend,
                persist: true,
            });
            config.holds.push(HoldConfig {
                id: HoldId::new("text_input_text"),
                initial: DdValue::text(""),
                triggered_by: vec![LinkId::new(link_id)],
                timer_interval_ms: 0,
                filter: EventFilter::TextStartsWith("Enter:".to_string()),
                transform: StateTransform::ClearText,
                persist: false,
            });
            // Add toggle HOLD for dynamic todo checkboxes
            // Uses a dedicated link "dynamic_todo_toggle" with events like "toggle:0"
            config.holds.push(HoldConfig {
                id: HoldId::new("todos"),
                initial: initial_list.clone(),
                triggered_by: vec![LinkId::new("dynamic_todo_toggle")],
                timer_interval_ms: 0,
                filter: EventFilter::TextStartsWith("toggle:".to_string()),
                transform: StateTransform::ToggleListItemCompleted,
                persist: true,
            });
            // HACK: TodoMVC-specific - Add editing HOLD for dynamic todo double-click
            // Uses events like "edit:0" (enter edit mode) or "unedit:0" (exit edit mode)
            config.holds.push(HoldConfig {
                id: HoldId::new("todos"),
                initial: initial_list.clone(),
                triggered_by: vec![LinkId::new("dynamic_todo_edit")],
                timer_interval_ms: 0,
                filter: EventFilter::Any,
                transform: StateTransform::SetListItemEditing,
                persist: true,
            });
            // HACK: TodoMVC-specific - Add save HOLD for dynamic todo title updates
            // Uses events like "save:0:new title"
            config.holds.push(HoldConfig {
                id: HoldId::new("todos"),
                initial: initial_list.clone(),
                triggered_by: vec![LinkId::new("dynamic_todo_save")],
                timer_interval_ms: 0,
                filter: EventFilter::TextStartsWith("save:".to_string()),
                transform: StateTransform::UpdateListItemTitle,
                persist: true,
            });
            // HACK: TodoMVC-specific - Add remove HOLD for dynamic todo deletion
            // Uses events like "remove:0"
            config.holds.push(HoldConfig {
                id: HoldId::new("todos"),
                initial: initial_list.clone(),
                triggered_by: vec![LinkId::new("dynamic_todo_remove")],
                timer_interval_ms: 0,
                filter: EventFilter::TextStartsWith("remove:".to_string()),
                transform: StateTransform::RemoveListItem,
                persist: true,
            });
            // Add Toggle All HOLD if toggle_all_checkbox is present
            // Triggers ListToggleAllCompleted on the todos list
            if let Some(ref toggle_all_id) = toggle_all_link {
                zoon::println!("[DD Interpreter] Adding toggle-all for todos: toggle_all_link={}", toggle_all_id);
                config.holds.push(HoldConfig {
                    id: HoldId::new("todos"),
                    initial: initial_list.clone(),
                    triggered_by: vec![LinkId::new(toggle_all_id)],
                    timer_interval_ms: 0,
                    filter: EventFilter::Any,
                    transform: StateTransform::ListToggleAllCompleted,
                    persist: true,
                });
            }
            // Add Clear Completed HOLD if remove_completed_button is present
            // Triggers ListRemoveCompleted on the todos list
            if let Some(ref clear_completed_id) = clear_completed_link {
                zoon::println!("[DD Interpreter] Adding clear-completed for todos: button_link={}", clear_completed_id);
                config.holds.push(HoldConfig {
                    id: HoldId::new("todos"),
                    initial: initial_list,
                    triggered_by: vec![LinkId::new(clear_completed_id)],
                    timer_interval_ms: 0,
                    filter: EventFilter::Any,
                    transform: StateTransform::ListRemoveCompleted,
                    persist: true,
                });
            }
        }
        config
    } else if let Some(ref link_id) = key_down_link {
        // Text input with key_down pattern (shopping_list): key_down |> WHEN { Enter => append }
        // Use persisted value, or fall back to static evaluation, or empty list
        let initial_list = load_persisted_hold_value("items")
            .unwrap_or_else(|| static_items.clone().unwrap_or_else(|| DdValue::List(std::sync::Arc::new(Vec::new()))));
        // Initialize HOLD_STATES so the bridge can read the initial value for reactive labels
        update_hold_state_no_persist("items", initial_list.clone());
        // Initialize text_input_text HOLD for reactive text clearing
        update_hold_state_no_persist("text_input_text", DdValue::text(""));

        // Check if there's also a clear button (List/clear pattern)
        if let Some(ref clear_link_id) = button_press_link {
            zoon::println!("[DD Interpreter] List-append-with-clear config: key_link={}, clear_link={}, initial {:?}",
                link_id, clear_link_id, initial_list);
            DataflowConfig::new().add_list_append_with_clear("items", initial_list, link_id, clear_link_id)
        } else {
            zoon::println!("[DD Interpreter] List-append config: link={}, initial {:?}", link_id, initial_list);
            DataflowConfig::new().add_list_append_on_enter("items", initial_list, link_id)
        }
    } else {
        // Link-driven pattern: button |> THEN |> HOLD/LATEST
        // Use "hold_0" to match the first HOLD ID generated by the evaluator
        let initial_value = load_persisted_hold_value("hold_0").unwrap_or_else(|| DdValue::int(0));
        // Initialize HOLD_STATES so the bridge can read the initial value
        update_hold_state_no_persist("hold_0", initial_value.clone());
        zoon::println!("[DD Interpreter] Link config: hold_0, initial {:?}", initial_value);
        DataflowConfig::counter_with_initial_hold("link_1", "hold_0", initial_value)
    };

    let worker_handle = DdWorker::with_config(config).spawn();

    // Split the handle to get all components
    let (event_input, document_output, task_handle) = worker_handle.split();

    // Set up global dispatcher so button clicks inject events
    let injector = EventInjector::new(event_input);
    set_global_dispatcher(injector.clone());

    // If timer-driven, start JavaScript timer to fire events
    if let Some((ref _hold_id, interval_ms)) = timer_info {
        let timer_injector = injector.clone();
        let timer_handle = Task::start_droppable(async move {
            let mut tick: u64 = 0;
            loop {
                zoon::Timer::sleep(interval_ms as u32).await;
                tick += 1;
                timer_injector.fire_timer(super::core::TimerId::new(interval_ms.to_string()), tick);
                zoon::println!("[DD Timer] Tick {} for {}ms timer", tick, interval_ms);
            }
        });
        // Store timer handle separately to keep it alive
        set_timer_handle(timer_handle);
        zoon::println!("[DD Interpreter] Timer started: {}ms interval", interval_ms);
    }

    // Store task handle to keep the async worker alive
    set_task_handle(task_handle);

    // Set up output listener to handle document updates
    let output_listener = Task::start_droppable(async move {
        let mut output_stream = document_output.stream();
        while let Some(update) = output_stream.next().await {
            zoon::println!("[DD Output] Received update: counter = {:?}", update.document);
            // Update the global HOLD states for reactive rendering
            // - hold_updates: update HOLD_STATES AND persist to localStorage
            for (hold_id, value) in update.hold_updates {
                update_hold_state(&hold_id, value);
            }
            // - hold_state_updates: update HOLD_STATES only, NO persistence
            for (hold_id, value) in &update.hold_state_updates {
                update_hold_state_no_persist(hold_id, value.clone());
            }
            // Clear text input DOM when text_input_text HOLD is updated
            // This implements the Boon pattern: text_to_add |> THEN { Text/empty() }
            if update.hold_state_updates.contains_key("text_input_text") {
                #[cfg(target_arch = "wasm32")]
                {
                    clear_dd_text_input_value();
                }
            }
        }
        zoon::println!("[DD Output] Output stream ended");
    });
    // Always store the output listener handle
    set_output_listener_handle(output_listener);

    zoon::println!("[DD Interpreter] DdWorker started, dispatcher and output listener configured");

    Some(DdResult {
        document,
        context: DdContext::new(),
    })
}

/// Extract timer info from a document if it contains a TimerRef.
///
/// Recursively searches the document for TimerRef values and returns
/// (hold_id, interval_ms) if found. Used to configure timer-driven reactivity.
fn extract_timer_info(document: &Option<DdValue>) -> Option<(String, u64)> {
    let doc = document.as_ref()?;
    extract_timer_info_from_value(doc)
}

/// Check if the document contains a text_input with key_down event (shopping_list pattern).
///
/// Returns the key_down link_id if found.
fn extract_text_input_key_down(document: &Option<DdValue>) -> Option<String> {
    let doc = document.as_ref()?;
    extract_key_down_from_value(doc)
}

/// Information about a checkbox toggle pattern.
/// Used for todo_mvc style patterns where checkbox clicks toggle boolean HOLDs.
struct CheckboxToggle {
    link_id: String,
    hold_id: String,
    initial: bool,
}

/// Extract checkbox toggle patterns from the document.
///
/// Looks for todo-like objects with:
///   - todo_elements.todo_checkbox → LinkRef (click event trigger)
///   - completed → HoldRef (boolean state)
fn extract_checkbox_toggles(document: &Option<DdValue>) -> Vec<CheckboxToggle> {
    let doc = match document.as_ref() {
        Some(d) => d,
        None => return Vec::new(),
    };
    let mut toggles = Vec::new();
    extract_checkbox_toggles_from_value(doc, &mut toggles);
    toggles
}

/// Recursively search for checkbox toggle patterns in the document.
///
/// Looks for Element/checkbox tags with:
///   - checked → HoldRef (the boolean state to toggle)
///   - element.event.click → LinkRef (the click trigger)
fn extract_checkbox_toggles_from_value(value: &DdValue, toggles: &mut Vec<CheckboxToggle>) {
    match value {
        DdValue::Tagged { tag, fields } if tag.as_ref() == "Element" => {
            // Check if this is a checkbox element
            if let Some(DdValue::Text(element_type)) = fields.get("_element_type") {
                if element_type.as_ref() == "checkbox" {
                    // Check for HoldRef in checked and LinkRef in element.event.click
                    if let Some(DdValue::HoldRef(hold_id)) = fields.get("checked") {
                        if let Some(element) = fields.get("element") {
                            if let Some(event) = element.get("event") {
                                if let Some(DdValue::LinkRef(link_id)) = event.get("click") {
                                    // Found a checkbox toggle pattern!
                                    let initial = load_persisted_hold_value(&hold_id.to_string())
                                        .map(|v| match v {
                                            DdValue::Bool(true) => true,
                                            DdValue::Tagged { tag, .. } if tag.as_ref() == "True" => true,
                                            _ => false,
                                        })
                                        .unwrap_or(false);
                                    toggles.push(CheckboxToggle {
                                        link_id: link_id.to_string(),
                                        hold_id: hold_id.to_string(),
                                        initial,
                                    });
                                    zoon::println!("[DD Interpreter] Found checkbox toggle: link={}, hold={}, initial={}",
                                        link_id, hold_id, initial);
                                }
                            }
                        }
                    }
                }
            }
            // Recurse into fields
            for (_, v) in fields.iter() {
                extract_checkbox_toggles_from_value(v, toggles);
            }
        }
        DdValue::Object(fields) => {
            // Recurse into all fields
            for (_, v) in fields.iter() {
                extract_checkbox_toggles_from_value(v, toggles);
            }
        }
        DdValue::List(items) => {
            for item in items.iter() {
                extract_checkbox_toggles_from_value(item, toggles);
            }
        }
        DdValue::Tagged { fields, .. } => {
            // Non-Element tagged values - still recurse
            for (_, v) in fields.iter() {
                extract_checkbox_toggles_from_value(v, toggles);
            }
        }
        _ => {}
    }
}

/// Information about an editing toggle pattern.
/// Used for todo_mvc style patterns where double-click enters editing mode.
struct EditingToggle {
    /// The HOLD ID that controls editing state (e.g., "hold_10")
    hold_id: String,
    /// The double_click LinkRef that sets editing to True
    double_click_link: String,
    /// The key_down LinkRef for Enter/Escape to exit editing
    key_down_link: Option<String>,
    /// The blur LinkRef to exit editing when focus is lost
    blur_link: Option<String>,
}

/// Extract editing toggle patterns from the document.
///
/// Looks for WhileRef patterns with:
///   - False arm: label element with element.event.double_click → LinkRef
///   - True arm: text_input with blur/key_down → LinkRef (for exiting edit mode)
fn extract_editing_toggles(document: &Option<DdValue>) -> Vec<EditingToggle> {
    let doc = match document.as_ref() {
        Some(d) => d,
        None => return Vec::new(),
    };
    let mut toggles = Vec::new();
    extract_editing_toggles_from_value(doc, &mut toggles);
    toggles
}

/// Recursively search for editing toggle patterns (WhileRef with label.double_click in False arm).
fn extract_editing_toggles_from_value(value: &DdValue, toggles: &mut Vec<EditingToggle>) {
    match value {
        DdValue::WhileRef { hold_id, arms, .. } => {
            // Check if this WhileRef has the editing pattern:
            // - False arm: label with double_click
            // - True arm: text_input with blur/key_down
            let mut double_click_link = None;
            let mut key_down_link = None;
            let mut blur_link = None;

            for (pattern, body) in arms.iter() {
                let is_false_arm = matches!(pattern, DdValue::Tagged { tag, .. } if tag.as_ref() == "False");
                let is_true_arm = matches!(pattern, DdValue::Tagged { tag, .. } if tag.as_ref() == "True");

                if is_false_arm {
                    // Look for label with double_click in the False arm
                    if let DdValue::Tagged { tag, fields } = body {
                        if tag.as_ref() == "Element" {
                            if let Some(DdValue::Text(element_type)) = fields.get("_element_type") {
                                if element_type.as_ref() == "label" {
                                    // Found label - check for double_click
                                    if let Some(element) = fields.get("element") {
                                        if let Some(event) = element.get("event") {
                                            if let Some(DdValue::LinkRef(link_id)) = event.get("double_click") {
                                                double_click_link = Some(link_id.to_string());
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                if is_true_arm {
                    // Look for text_input with blur/key_down in the True arm
                    if let DdValue::Tagged { tag, fields } = body {
                        if tag.as_ref() == "Element" {
                            if let Some(DdValue::Text(element_type)) = fields.get("_element_type") {
                                if element_type.as_ref() == "text_input" {
                                    // Found text_input - check for blur and key_down
                                    if let Some(element) = fields.get("element") {
                                        if let Some(event) = element.get("event") {
                                            if let Some(DdValue::LinkRef(link_id)) = event.get("blur") {
                                                blur_link = Some(link_id.to_string());
                                            }
                                            if let Some(DdValue::LinkRef(link_id)) = event.get("key_down") {
                                                key_down_link = Some(link_id.to_string());
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // If we found a double_click link, this is an editing toggle pattern
            if let Some(dbl_click) = double_click_link {
                zoon::println!("[DD Interpreter] Found editing toggle: hold={}, double_click={}, key_down={:?}, blur={:?}",
                    hold_id, dbl_click, key_down_link, blur_link);
                toggles.push(EditingToggle {
                    hold_id: hold_id.to_string(),
                    double_click_link: dbl_click,
                    key_down_link,
                    blur_link,
                });
            }

            // Also recurse into arms to find nested patterns
            for (_, body) in arms.iter() {
                extract_editing_toggles_from_value(body, toggles);
            }
        }
        DdValue::Tagged { fields, .. } => {
            for (_, v) in fields.iter() {
                extract_editing_toggles_from_value(v, toggles);
            }
        }
        DdValue::Object(fields) => {
            for (_, v) in fields.iter() {
                extract_editing_toggles_from_value(v, toggles);
            }
        }
        DdValue::List(items) => {
            for item in items.iter() {
                extract_editing_toggles_from_value(item, toggles);
            }
        }
        _ => {}
    }
}

/// Extract the toggle_all checkbox click link_id.
///
/// Looks for a checkbox whose `checked` field is NOT a HoldRef (individual todo checkboxes have HoldRef).
/// The toggle_all checkbox has `checked: all_completed` which is a computed Bool, not a HoldRef.
fn extract_toggle_all_link(document: &Option<DdValue>) -> Option<String> {
    let doc = document.as_ref()?;
    extract_toggle_all_from_value(doc)
}

/// Recursively search for toggle_all checkbox.
/// The toggle_all checkbox is identified by having `checked` as a Bool (computed), not a HoldRef.
fn extract_toggle_all_from_value(value: &DdValue) -> Option<String> {
    match value {
        DdValue::Tagged { tag, fields } if tag.as_ref() == "Element" => {
            // Check if this is a checkbox
            if let Some(DdValue::Text(element_type)) = fields.get("_element_type") {
                if element_type.as_ref() == "checkbox" {
                    // Check if `checked` is NOT a HoldRef (toggle_all has computed Bool)
                    if let Some(checked) = fields.get("checked") {
                        let is_hold_ref = matches!(checked, DdValue::HoldRef(_));
                        if !is_hold_ref {
                            // This is likely the toggle_all checkbox - get its click link
                            if let Some(element) = fields.get("element") {
                                if let Some(event) = element.get("event") {
                                    if let Some(DdValue::LinkRef(link_id)) = event.get("click") {
                                        zoon::println!("[DD Interpreter] Found toggle_all checkbox: link_id={}, checked={:?}", link_id, checked);
                                        return Some(link_id.to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }
            // Recurse into fields
            for (_, v) in fields.iter() {
                if let Some(id) = extract_toggle_all_from_value(v) {
                    return Some(id);
                }
            }
            None
        }
        DdValue::Object(fields) => {
            for (_, v) in fields.iter() {
                if let Some(id) = extract_toggle_all_from_value(v) {
                    return Some(id);
                }
            }
            None
        }
        DdValue::List(items) => {
            for item in items.iter() {
                if let Some(id) = extract_toggle_all_from_value(item) {
                    return Some(id);
                }
            }
            None
        }
        DdValue::Tagged { fields, .. } => {
            for (_, v) in fields.iter() {
                if let Some(id) = extract_toggle_all_from_value(v) {
                    return Some(id);
                }
            }
            None
        }
        _ => None,
    }
}

/// Recursively search for text_input with key_down LinkRef.
fn extract_key_down_from_value(value: &DdValue) -> Option<String> {
    match value {
        DdValue::Tagged { tag, fields } if tag.as_ref() == "Element" => {
            // Check if this is a text_input
            if let Some(DdValue::Text(t)) = fields.get("_element_type") {
                if t.as_ref() == "text_input" {
                    // Look for element.event.key_down LinkRef
                    if let Some(element) = fields.get("element") {
                        if let Some(event) = element.get("event") {
                            if let Some(DdValue::LinkRef(link_id)) = event.get("key_down") {
                                return Some(link_id.to_string());
                            }
                        }
                    }
                }
            }
            // Recurse into fields
            for (_, v) in fields.iter() {
                if let Some(id) = extract_key_down_from_value(v) {
                    return Some(id);
                }
            }
            None
        }
        DdValue::List(items) => {
            for item in items.iter() {
                if let Some(id) = extract_key_down_from_value(item) {
                    return Some(id);
                }
            }
            None
        }
        DdValue::Object(fields) => {
            for (_, v) in fields.iter() {
                if let Some(id) = extract_key_down_from_value(v) {
                    return Some(id);
                }
            }
            None
        }
        DdValue::Tagged { fields, .. } => {
            for (_, v) in fields.iter() {
                if let Some(id) = extract_key_down_from_value(v) {
                    return Some(id);
                }
            }
            None
        }
        _ => None,
    }
}

/// Check if the document contains a button with press event (for List/clear pattern).
///
/// Returns the press link_id if found.
fn extract_button_press_link(document: &Option<DdValue>) -> Option<String> {
    let doc = document.as_ref()?;
    extract_button_press_from_value(doc, None)
}

/// Extract "Clear completed" button specifically for todo_mvc.
/// Looks for a button with label "Clear completed".
fn extract_clear_completed_button_link(document: &Option<DdValue>) -> Option<String> {
    let doc = document.as_ref()?;
    extract_button_press_from_value(doc, Some("Clear completed"))
}

/// Recursively search for button with press LinkRef.
/// If `label_filter` is Some, only match buttons with that exact label.
fn extract_button_press_from_value(value: &DdValue, label_filter: Option<&str>) -> Option<String> {
    match value {
        DdValue::Tagged { tag, fields } if tag.as_ref() == "Element" => {
            // Check if this is a button
            if let Some(DdValue::Text(t)) = fields.get("_element_type") {
                if t.as_ref() == "button" {
                    // If label_filter is set, check that the button label matches
                    if let Some(expected_label) = label_filter {
                        if let Some(DdValue::Text(label)) = fields.get("label") {
                            if label.as_ref() != expected_label {
                                // Label doesn't match, continue searching
                                // Recurse into fields
                                for (_, v) in fields.iter() {
                                    if let Some(id) = extract_button_press_from_value(v, label_filter) {
                                        return Some(id);
                                    }
                                }
                                return None;
                            }
                        } else {
                            // No label field, continue searching
                            for (_, v) in fields.iter() {
                                if let Some(id) = extract_button_press_from_value(v, label_filter) {
                                    return Some(id);
                                }
                            }
                            return None;
                        }
                    }
                    // Look for element.event.press LinkRef
                    if let Some(element) = fields.get("element") {
                        if let Some(event) = element.get("event") {
                            if let Some(DdValue::LinkRef(link_id)) = event.get("press") {
                                zoon::println!("[DD Interpreter] Found button press link: {} (label filter: {:?})", link_id, label_filter);
                                return Some(link_id.to_string());
                            }
                        }
                    }
                }
            }
            // Recurse into fields
            for (_, v) in fields.iter() {
                if let Some(id) = extract_button_press_from_value(v, label_filter) {
                    return Some(id);
                }
            }
            None
        }
        DdValue::List(items) => {
            for item in items.iter() {
                if let Some(id) = extract_button_press_from_value(item, label_filter) {
                    return Some(id);
                }
            }
            None
        }
        DdValue::Object(fields) => {
            for (_, v) in fields.iter() {
                if let Some(id) = extract_button_press_from_value(v, label_filter) {
                    return Some(id);
                }
            }
            None
        }
        DdValue::Tagged { fields, .. } => {
            for (_, v) in fields.iter() {
                if let Some(id) = extract_button_press_from_value(v, label_filter) {
                    return Some(id);
                }
            }
            None
        }
        // Also search inside WhileRef arms - the button might be conditionally rendered
        DdValue::WhileRef { arms, default, .. } => {
            for (_, arm_value) in arms.iter() {
                if let Some(id) = extract_button_press_from_value(arm_value, label_filter) {
                    return Some(id);
                }
            }
            if let Some(default_value) = default {
                if let Some(id) = extract_button_press_from_value(default_value, label_filter) {
                    return Some(id);
                }
            }
            None
        }
        _ => None,
    }
}

/// Recursively search a DdValue for TimerRef.
fn extract_timer_info_from_value(value: &DdValue) -> Option<(String, u64)> {
    match value {
        DdValue::TimerRef { id, interval_ms } => {
            Some((id.to_string(), *interval_ms))
        }
        DdValue::List(items) => {
            for item in items.iter() {
                if let Some(info) = extract_timer_info_from_value(item) {
                    return Some(info);
                }
            }
            None
        }
        DdValue::Object(fields) => {
            for (_, v) in fields.iter() {
                if let Some(info) = extract_timer_info_from_value(v) {
                    return Some(info);
                }
            }
            None
        }
        DdValue::Tagged { fields, .. } => {
            for (_, v) in fields.iter() {
                if let Some(info) = extract_timer_info_from_value(v) {
                    return Some(info);
                }
            }
            None
        }
        // Other value types don't contain TimerRef
        DdValue::Unit | DdValue::Bool(_) | DdValue::Number(_) | DdValue::Text(_)
        | DdValue::HoldRef(_) | DdValue::LinkRef(_) | DdValue::WhileRef { .. }
        | DdValue::ComputedRef { .. } | DdValue::FilteredListRef { .. }
        | DdValue::ReactiveFilteredList { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_evaluation() {
        let code = r#"
            document: 42
        "#;
        let result = run_dd_reactive_with_persistence("test.bn", code, None);
        assert!(result.is_some());
        let result = result.unwrap();
        assert!(result.document.is_some());
    }
}
