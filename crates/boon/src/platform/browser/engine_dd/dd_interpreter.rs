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
use super::core::{DdWorker, DataflowConfig, HoldConfig, HoldId, LinkId, EventFilter, StateTransform, reconstruct_persisted_item, instantiate_fresh_item};
use super::io::{EventInjector, set_global_dispatcher, clear_global_dispatcher, set_task_handle, clear_task_handle, set_output_listener_handle, clear_output_listener_handle, set_timer_handle, clear_timer_handle, clear_router_mappings, clear_dynamic_link_actions, update_hold_state, update_hold_state_no_persist, load_persisted_hold_value, set_checkbox_toggle_holds, clear_checkbox_toggle_holds, clear_hold_states_memory, set_list_var_name, clear_remove_event_path, clear_bulk_remove_event_path, get_editing_event_bindings, clear_editing_event_bindings, get_toggle_event_bindings, clear_toggle_event_bindings, get_global_toggle_bindings, clear_global_toggle_bindings, get_bulk_remove_event_path, get_text_input_key_down_link, clear_text_input_key_down_link, get_list_clear_link, clear_list_clear_link, get_has_template_list, clear_has_template_list};
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
/// Check if a DdValue contains HoldRefs (indicating it uses item data).
/// Used to distinguish element templates that use item data (like todo_item)
/// from container elements that don't (like main_panel).
fn has_dynamic_holds(value: &DdValue) -> bool {
    match value {
        DdValue::HoldRef(_) => true,
        DdValue::List(items) => items.iter().any(has_dynamic_holds),
        DdValue::Object(fields) => fields.values().any(has_dynamic_holds),
        DdValue::Tagged { fields, .. } => fields.values().any(has_dynamic_holds),
        DdValue::WhileRef { arms, default, .. } => {
            // Check arms and default for HoldRefs
            arms.iter().any(|(_, body)| has_dynamic_holds(body)) ||
            default.as_ref().map_or(false, |d| has_dynamic_holds(d.as_ref()))
        }
        _ => false,
    }
}

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
    clear_dynamic_link_actions();  // Clear dynamic link→hold mappings
    clear_remove_event_path();  // Clear parsed remove event path
    clear_bulk_remove_event_path();  // Clear parsed bulk remove event path
    clear_editing_event_bindings();  // Clear parsed editing event bindings
    clear_toggle_event_bindings();  // Clear parsed toggle event bindings
    clear_global_toggle_bindings();  // Clear parsed global toggle event bindings
    clear_checkbox_toggle_holds();  // Clear checkbox toggle hold tracking
    super::io::clear_text_clear_holds();  // Task 7.1: Clear text-clear HOLD registry
    clear_text_input_key_down_link();  // Clear text_input key_down LinkRef
    clear_list_clear_link();  // Clear List/clear event LinkRef
    clear_has_template_list();  // Clear template list flag
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

    // Task 4.4: Get the DataflowConfig built during evaluation
    // This config contains HoldConfig entries added by eval_hold()
    let evaluator_config = runtime.take_config();
    zoon::println!("[DD Interpreter] Evaluator built {} HoldConfig entries", evaluator_config.holds.len());
    for (i, hold) in evaluator_config.holds.iter().enumerate() {
        zoon::println!("[DD Interpreter]   [{}] id={}, transform={:?}, timer={}ms",
            i, hold.id.name(), hold.transform, hold.timer_interval_ms);
    }

    // Get the initial list from static evaluation
    // Detect the list variable dynamically by looking for variables containing List values
    // Common patterns: store.items, store.list_data, items, list_data, or any variable containing a List
    let (static_list, list_var_name) = detect_list_variable(&runtime);
    zoon::println!("[DD Interpreter] Detected list variable: {:?}", list_var_name);
    // Task 7.1: Only set list_var_name if actually detected (no hardcoded fallback)
    let list_hold_name = list_var_name.clone();
    if let Some(ref name) = list_hold_name {
        set_list_var_name(name.clone());
    }
    // Task 7.1: Use detected list variable instead of hardcoded "items"
    let static_items = static_list.clone();

    zoon::println!("[DD Interpreter] Evaluation complete, has document: {}",
        document.is_some());
    zoon::println!("[DD Interpreter] static_list = {:?}", static_list);
    zoon::println!("[DD Interpreter] static_items = {:?}", static_items);

    // Step 7: Set up DdWorker for reactive updates
    // Task 4.3: Prefer evaluator-built config over extract_* pattern detection

    // Check if evaluator built timer HOLDs (timer_interval_ms > 0)
    let has_timer_hold = evaluator_config.holds.iter().any(|h| h.timer_interval_ms > 0);

    // Check if evaluator built link-triggered HOLDs with BoolToggle transform
    // These come from extract_toggle_bindings_with_link_ids() in the evaluator
    let toggle_bindings = get_toggle_event_bindings();
    let has_evaluator_toggle_holds = !toggle_bindings.is_empty() &&
        toggle_bindings.iter().any(|b| b.link_id.is_some());
    zoon::println!("[DD Interpreter] Toggle bindings from evaluator: {} (has_link_ids: {})",
        toggle_bindings.len(), has_evaluator_toggle_holds);

    // Check if evaluator provided editing bindings with link_ids
    let editing_bindings = get_editing_event_bindings();
    let has_evaluator_editing_bindings = editing_bindings.edit_trigger_link_id.is_some()
        || editing_bindings.exit_key_link_id.is_some();
    zoon::println!("[DD Interpreter] Editing bindings from evaluator: hold_id={:?}, has_link_ids={}",
        editing_bindings.hold_id, has_evaluator_editing_bindings);

    // Task 6.3: Get timer info from evaluator-built config ONLY (no fallback)
    let timer_info: Option<(String, u64)> = evaluator_config.holds.iter()
        .find(|h| h.timer_interval_ms > 0)
        .map(|h| (h.id.name().to_string(), h.timer_interval_ms));
    // Task 6.3: Get key_down link from evaluator ONLY (no fallback)
    // LinkSetter now detects text_input key_down with the final link ID after replacement
    let key_down_link = get_text_input_key_down_link();
    if key_down_link.is_some() {
        zoon::println!("[DD Interpreter] Using evaluator-provided text_input key_down link: {:?}", key_down_link);
    }
    // Task 6.3: Get List/clear link from evaluator ONLY (no fallback)
    // Evaluator extracts LinkRef from List/clear(on: ...) during evaluation
    let button_press_link = get_list_clear_link();
    if button_press_link.is_some() {
        zoon::println!("[DD Interpreter] Using evaluator-provided List/clear link: {:?}", button_press_link);
    }
    // Task 6.3: Checkbox toggles now come from evaluator ONLY (no fallback)
    // The evaluator detects checkbox patterns during HOLD evaluation
    let checkbox_toggles: Vec<CheckboxToggle> = Vec::new();
    // Task 6.3: Editing toggles now come from evaluator ONLY (no fallback)
    // The evaluator detects editing patterns during HOLD evaluation
    let editing_toggles: Vec<EditingToggle> = Vec::new();
    // NOTE: clear_completed_link is no longer extracted by label matching!
    // Instead, we use get_bulk_remove_event_path() which is set from parsed List/remove pattern

    let config = if has_timer_hold {
        // Task 4.3: Use evaluator-built config for timer patterns
        // This eliminates extract_timer_info() for timer-driven patterns
        zoon::println!("[DD Interpreter] Using evaluator-built config for timer pattern ({} holds)", evaluator_config.holds.len());
        evaluator_config
    } else if has_evaluator_toggle_holds {
        // Task 4.3: Use evaluator-built config for toggle patterns
        // Populate triggered_by from toggle bindings which now have link_ids
        zoon::println!("[DD Interpreter] Using evaluator-built config for toggle pattern ({} holds)", evaluator_config.holds.len());
        let mut config = evaluator_config;

        // Populate triggered_by for each HoldConfig from toggle bindings
        for hold_config in &mut config.holds {
            // Find toggle binding for this hold
            for binding in &toggle_bindings {
                if binding.hold_id == hold_config.id.name() {
                    if let Some(ref link_id) = binding.link_id {
                        zoon::println!("[DD Interpreter] Populating triggered_by for {}: {}",
                            hold_config.id.name(), link_id);
                        hold_config.triggered_by.push(LinkId::new(link_id));
                    }
                }
            }
        }

        // Register hold IDs for reactive count (like checkbox_hold_ids)
        let checkbox_hold_ids: Vec<String> = toggle_bindings.iter()
            .map(|b| b.hold_id.clone())
            .collect();
        if !checkbox_hold_ids.is_empty() {
            set_checkbox_toggle_holds(checkbox_hold_ids);
        }

        // Task 4.3: Add editing bindings from evaluator (SetTrue/SetFalse for edit mode)
        if has_evaluator_editing_bindings {
            if let Some(ref editing_hold_id) = editing_bindings.hold_id {
                // SetTrue triggered by double_click
                if let Some(ref link_id) = editing_bindings.edit_trigger_link_id {
                    zoon::println!("[DD Interpreter] Adding SetTrue for editing: {} triggered by {}", editing_hold_id, link_id);
                    config.holds.push(HoldConfig {
                        id: HoldId::new(editing_hold_id),
                        initial: DdValue::Bool(false),
                        triggered_by: vec![LinkId::new(link_id)],
                        timer_interval_ms: 0,
                        filter: EventFilter::Any,
                        transform: StateTransform::SetTrue,
                        persist: false,
                    });
                }
                // SetFalse triggered by key_down (Enter/Escape)
                if let Some(ref link_id) = editing_bindings.exit_key_link_id {
                    zoon::println!("[DD Interpreter] Adding SetFalse (Enter) for editing: {} triggered by {}", editing_hold_id, link_id);
                    config.holds.push(HoldConfig {
                        id: HoldId::new(editing_hold_id),
                        initial: DdValue::Bool(false),
                        triggered_by: vec![LinkId::new(link_id)],
                        timer_interval_ms: 0,
                        filter: EventFilter::TextEquals("Enter".to_string()),
                        transform: StateTransform::SetFalse,
                        persist: false,
                    });
                    config.holds.push(HoldConfig {
                        id: HoldId::new(editing_hold_id),
                        initial: DdValue::Bool(false),
                        triggered_by: vec![LinkId::new(link_id)],
                        timer_interval_ms: 0,
                        filter: EventFilter::TextEquals("Escape".to_string()),
                        transform: StateTransform::SetFalse,
                        persist: false,
                    });
                }
                // Initialize the editing HOLD state
                update_hold_state_no_persist(editing_hold_id, DdValue::Bool(false));
            }
        }

        config
    } else if let Some((ref hold_id, interval_ms)) = timer_info {
        // Legacy: Timer-driven pattern detected via extract_timer_info
        // (This branch should no longer be reached for timer patterns)
        let initial_value = DdValue::int(0);
        zoon::println!("[DD Interpreter] Timer config: {} @ {}ms, initial {:?}", hold_id, interval_ms, initial_value);
        DataflowConfig::timer_counter(hold_id, initial_value, interval_ms)
    } else if !checkbox_toggles.is_empty() || get_has_template_list() {
        // Checkbox toggle pattern (list_example) or template-based list (todo_mvc):
        // - list_example: checkbox.click |> THEN { state |> Bool/not() } - static checkboxes
        // - todo_mvc: FilteredMappedListWithPredicate - checkboxes inside templates use PlaceholderField
        // Each checkbox has its own HOLD for the completed state
        // Task 6.3: Use evaluator-provided flag instead of document scanning
        let has_template_list = get_has_template_list();
        zoon::println!("[DD Interpreter] Checkbox/template config: {} toggles, has_template_list: {}", checkbox_toggles.len(), has_template_list);
        let mut config = DataflowConfig::new();
        let mut checkbox_hold_ids = Vec::new();
        for toggle in &checkbox_toggles {
            // Initialize HOLD_STATES for reactive rendering
            update_hold_state_no_persist(&toggle.hold_id, DdValue::Bool(toggle.initial));
            checkbox_hold_ids.push(toggle.hold_id.clone());
            // Only trigger on own checkbox click
            // (toggle_all is handled via HOLD body subscriptions in the Boon code)
            let triggers = vec![LinkId::new(&toggle.link_id)];
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

        // Add editing toggle HOLDs (for double-click to edit in list_example)
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

        // Also check for text input key_down pattern (list_example has BOTH checkboxes AND add-item input)
        // Task 7.1: Only process if we have both a key_down link AND a detected list variable
        if let (Some(link_id), Some(list_name)) = (&key_down_link, &list_hold_name) {
            // Use persisted value, or fall back to in-memory HOLD state (set by eval_object), or empty list
            let initial_list = load_persisted_hold_value(list_name)
                .unwrap_or_else(|| {
                    match &static_list {
                        Some(DdValue::List(_)) => static_list.clone().unwrap(),
                        // HoldRef: eval_object already stored initial value in HOLD_STATES
                        Some(DdValue::HoldRef(hold_id)) => {
                            super::io::get_hold_value(hold_id.as_ref())
                                .unwrap_or_else(|| DdValue::List(std::sync::Arc::new(Vec::new())))
                        }
                        _ => DdValue::List(std::sync::Arc::new(Vec::new())),
                    }
                });
            zoon::println!("[DD Interpreter] list initial_list: {:?}", initial_list);
            // Initialize HOLD_STATES for reactive rendering
            update_hold_state_no_persist(list_name, initial_list.clone());
            // Initialize text-clear HOLD for reactive text clearing (Task 7.1: dynamic name from link ID)
            let text_clear_hold_id = format!("text_clear_{}", link_id);
            super::io::add_text_clear_hold(text_clear_hold_id.clone());
            update_hold_state_no_persist(&text_clear_hold_id, DdValue::text(""));
            zoon::println!("[DD Interpreter] Also adding list-append for list: link={}, text_clear={}", link_id, text_clear_hold_id);

            // Register dynamic link actions for initial items
            // This allows the same mechanisms to work for both initial and dynamic items
            // NOTE: Use static_list (not initial_list) because persisted data doesn't include LinkRefs
            if let Some(DdValue::List(items)) = &static_list {
                use super::io::{add_dynamic_link_action, DynamicLinkAction};
                for item in items.iter() {
                    if let DdValue::Object(obj) = item {
                        // DEBUG: Log the item object structure
                        zoon::println!("[DD Interpreter] Initial item object fields: {:?}", obj.keys().collect::<Vec<_>>());

                        // Discover HoldRef fields dynamically by type
                        // Find boolean HoldRef (editing state) and text HoldRef (title data)
                        let mut editing_hold: Option<String> = None;
                        let mut title_hold: Option<String> = None;
                        for (hold_field, hold_value) in obj.iter() {
                            if let DdValue::HoldRef(hold_id) = hold_value {
                                // Check the initial value to determine type
                                if let Some(initial) = super::io::get_hold_value(hold_id) {
                                    match initial {
                                        DdValue::Bool(_) | DdValue::Tagged { .. } => {
                                            // Boolean or Tagged (True/False) - this is the editing state
                                            zoon::println!("[DD Interpreter] Initial item: detected boolean HoldRef: {} = {}", hold_field, hold_id);
                                            editing_hold = Some(hold_id.to_string());
                                        }
                                        DdValue::Text(_) => {
                                            // Text - this is the title
                                            zoon::println!("[DD Interpreter] Initial item: detected text HoldRef: {} = {}", hold_field, hold_id);
                                            title_hold = Some(hold_id.to_string());
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                        zoon::println!("[DD Interpreter] Discovered editing_hold={:?}, title_hold={:?}", editing_hold, title_hold);

                        // Find the elements field dynamically (Object containing LinkRefs)
                        let elements_field = obj.iter()
                            .find(|(_, v)| matches!(v, DdValue::Object(inner) if inner.values().any(|iv| matches!(iv, DdValue::LinkRef(_)))))
                            .map(|(k, v)| (k.clone(), v.clone()));

                        if let Some((elements_name, DdValue::Object(item_elements))) = elements_field {
                            zoon::println!("[DD Interpreter] Found elements field '{}' with {} LinkRefs", elements_name, item_elements.len());

                            // Register actions using PARSED PATH from List/remove(item, on: ...)
                            // Get the remove event path that was parsed from the Boon code
                            let remove_path = super::io::get_remove_event_path();
                            zoon::println!("[DD Interpreter] Using parsed remove path: {:?}", remove_path);

                            // Register remove action using parsed path (no pattern matching!)
                            if remove_path.len() >= 2 {
                                // Path is like ["todo_elements", "remove_todo_button"]
                                // First element is the field we're currently in (elements_name)
                                // Check if it matches and navigate to the LinkRef
                                if remove_path[0] == elements_name.as_ref() {
                                    if let Some(DdValue::LinkRef(link_id)) = item_elements.get(remove_path[1].as_str()) {
                                        zoon::println!("[DD Interpreter] Initial item: {} -> RemoveListItem (via parsed path)", link_id);
                                        add_dynamic_link_action(link_id.to_string(), DynamicLinkAction::RemoveListItem { link_id: link_id.to_string() });
                                    }
                                }
                            }

                            // Register editing actions using PARSED PATHS from HOLD body (no pattern matching!)
                            let editing_bindings = get_editing_event_bindings();
                            zoon::println!("[DD Interpreter] Using parsed editing bindings: edit_trigger={:?}, exit_key={:?}, exit_blur={:?}",
                                editing_bindings.edit_trigger_path, editing_bindings.exit_key_path, editing_bindings.exit_blur_path);

                            // Double-click element (edit_trigger_path) -> SetTrue(editing_hold)
                            if editing_bindings.edit_trigger_path.len() >= 2 && editing_bindings.edit_trigger_path[0] == elements_name.as_ref() {
                                if let Some(DdValue::LinkRef(link_id)) = item_elements.get(editing_bindings.edit_trigger_path[1].as_str()) {
                                    if let Some(ref edit_hold) = editing_hold {
                                        zoon::println!("[DD Interpreter] Initial item: {} -> SetTrue({}) (via parsed path)", link_id, edit_hold);
                                        add_dynamic_link_action(link_id.to_string(), DynamicLinkAction::SetTrue(edit_hold.clone()));
                                    }
                                }
                            }

                            // Key/Blur element (exit_key_path) -> EditingHandler
                            if editing_bindings.exit_key_path.len() >= 2 && editing_bindings.exit_key_path[0] == elements_name.as_ref() {
                                if let Some(DdValue::LinkRef(link_id)) = item_elements.get(editing_bindings.exit_key_path[1].as_str()) {
                                    if let (Some(edit_hold), Some(t_hold)) = (&editing_hold, &title_hold) {
                                        zoon::println!("[DD Interpreter] Initial item: {} -> EditingHandler(edit={}, title={}) (via parsed path)", link_id, edit_hold, t_hold);
                                        add_dynamic_link_action(link_id.to_string(), DynamicLinkAction::EditingHandler {
                                            editing_hold: edit_hold.clone(),
                                            title_hold: t_hold.clone(),
                                        });
                                    }
                                }
                            }

                            // Register toggle bindings from HOLD body parsing (click |> THEN { state |> Bool/not() })
                            // Each binding has a specific hold_id that was created during item evaluation
                            let toggle_bindings = get_toggle_event_bindings();
                            for binding in &toggle_bindings {
                                // Check if the binding path matches this item's elements
                                if binding.event_path.len() >= 2 && binding.event_path[0] == elements_name.as_ref() {
                                    if let Some(DdValue::LinkRef(link_id)) = item_elements.get(binding.event_path[1].as_str()) {
                                        // Check if this item contains the binding's hold_id
                                        // The binding was created during this item's evaluation, so hold_id should match
                                        let item_has_this_hold = obj.values().any(|v| {
                                            matches!(v, DdValue::HoldRef(id) if id.as_ref() == binding.hold_id)
                                        });
                                        if item_has_this_hold {
                                            zoon::println!("[DD Interpreter] Initial item: {} -> BoolToggle({}) (via parsed toggle binding)", link_id, binding.hold_id);
                                            add_dynamic_link_action(link_id.to_string(), DynamicLinkAction::BoolToggle(binding.hold_id.clone()));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Try to create both templates by detecting available functions based on OUTPUT:
            // 1. data_template: A function that returns an Object with HoldRef fields
            // 2. element_template: A function that returns a Tagged Element
            // This makes the engine truly generic - no assumptions about naming conventions
            let func_names: Vec<String> = runtime.get_function_names().into_iter().cloned().collect();

            // Find data template function by testing each function's output
            let mut data_template: Option<DdValue> = None;
            let mut data_func_name: Option<String> = None;
            for name in &func_names {
                // Get actual parameter names from the function definition
                // Clone the first param to release the borrow before calling call_function
                let first_param = runtime.get_function_parameters(name)
                    .and_then(|params| params.first().cloned());
                // Only try functions that take a parameter (data templates take an item parameter)
                if let Some(first_param) = first_param {
                        // Try calling with the actual parameter name
                        if let Some(result) = runtime.call_function(name, &[(first_param.as_str(), DdValue::text("__TEMPLATE__"))]) {
                            // Check if result is an Object with HoldRef fields (indicates a list item template)
                            if let DdValue::Object(fields) = &result {
                                let has_hold_refs = fields.values().any(|v| matches!(v, DdValue::HoldRef(_)));
                                if has_hold_refs {
                                    zoon::println!("[DD Interpreter] Found data template function: {} -> {:?}", name, result);
                                    // Detect the elements field name (Object field containing LinkRefs)
                                    // and register actions for template's LinkRefs
                                    for (field_name, field_value) in fields.iter() {
                                        if let DdValue::Object(inner_fields) = field_value {
                                            let has_link_refs = inner_fields.values().any(|v| matches!(v, DdValue::LinkRef(_)));
                                            if has_link_refs {
                                                zoon::println!("[DD Interpreter] Detected elements field name: {}", field_name);
                                                super::io::set_elements_field_name(field_name.to_string());

                                                // Discover HoldRef fields dynamically by type
                                                // Find boolean HoldRefs (editing AND completed) and text HoldRef (title)
                                                let mut editing_hold: Option<String> = None;
                                                let mut completed_hold: Option<String> = None;
                                                let mut title_hold: Option<String> = None;
                                                for (hold_field, hold_value) in fields.iter() {
                                                    if let DdValue::HoldRef(hold_id) = hold_value {
                                                        // Check the initial value to determine type
                                                        if let Some(initial) = super::io::get_hold_value(hold_id) {
                                                            match initial {
                                                                DdValue::Bool(_) | DdValue::Tagged { .. } => {
                                                                    // Boolean or Tagged (True/False)
                                                                    // Distinguish by field name: "editing" vs "completed"
                                                                    if hold_field.contains("edit") {
                                                                        zoon::println!("[DD Interpreter] Detected editing HoldRef: {} = {}", hold_field, hold_id);
                                                                        editing_hold = Some(hold_id.to_string());
                                                                    } else {
                                                                        zoon::println!("[DD Interpreter] Detected completed HoldRef: {} = {}", hold_field, hold_id);
                                                                        completed_hold = Some(hold_id.to_string());
                                                                    }
                                                                }
                                                                DdValue::Text(_) => {
                                                                    // Text - this is the title
                                                                    zoon::println!("[DD Interpreter] Detected text HoldRef: {} = {}", hold_field, hold_id);
                                                                    title_hold = Some(hold_id.to_string());
                                                                }
                                                                _ => {}
                                                            }
                                                        }
                                                    }
                                                }

                                                use super::io::{add_dynamic_link_action, DynamicLinkAction};

                                                // Use PARSED PATH from List/remove for remove action (no pattern matching!)
                                                let remove_path = super::io::get_remove_event_path();
                                                zoon::println!("[DD Interpreter] Template using parsed remove path: {:?}", remove_path);

                                                // Register remove action using parsed path
                                                if remove_path.len() >= 2 && remove_path[0] == field_name.as_ref() {
                                                    if let Some(DdValue::LinkRef(link_id)) = inner_fields.get(remove_path[1].as_str()) {
                                                        zoon::println!("[DD Interpreter] Template action: {} -> RemoveListItem (via parsed path)", link_id);
                                                        add_dynamic_link_action(link_id.to_string(), DynamicLinkAction::RemoveListItem { link_id: link_id.to_string() });
                                                    }
                                                }

                                                // Register editing actions using PARSED PATHS from HOLD body (no pattern matching!)
                                                let editing_bindings = get_editing_event_bindings();

                                                // Double-click element (edit_trigger_path) -> SetTrue(editing_hold)
                                                if editing_bindings.edit_trigger_path.len() >= 2 && editing_bindings.edit_trigger_path[0] == field_name.as_ref() {
                                                    if let Some(DdValue::LinkRef(link_id)) = inner_fields.get(editing_bindings.edit_trigger_path[1].as_str()) {
                                                        if let Some(ref edit_hold) = editing_hold {
                                                            zoon::println!("[DD Interpreter] Template action: {} -> SetTrue({}) (via parsed path)", link_id, edit_hold);
                                                            add_dynamic_link_action(link_id.to_string(), DynamicLinkAction::SetTrue(edit_hold.clone()));
                                                        }
                                                    }
                                                }

                                                // Key/Blur element (exit_key_path) -> EditingHandler
                                                if editing_bindings.exit_key_path.len() >= 2 && editing_bindings.exit_key_path[0] == field_name.as_ref() {
                                                    if let Some(DdValue::LinkRef(link_id)) = inner_fields.get(editing_bindings.exit_key_path[1].as_str()) {
                                                        if let (Some(edit_hold), Some(t_hold)) = (&editing_hold, &title_hold) {
                                                            zoon::println!("[DD Interpreter] Template action: {} -> EditingHandler(edit={}, title={}) (via parsed path)", link_id, edit_hold, t_hold);
                                                            add_dynamic_link_action(link_id.to_string(), DynamicLinkAction::EditingHandler {
                                                                editing_hold: edit_hold.clone(),
                                                                title_hold: t_hold.clone(),
                                                            });
                                                        }
                                                    }
                                                }

                                                // Register toggle bindings from HOLD body parsing
                                                // Toggle bindings indicate checkbox -> completed toggle patterns
                                                // Use the detected completed_hold (not the binding's hold_id which is from existing items)
                                                let toggle_bindings = get_toggle_event_bindings();
                                                zoon::println!("[DD Interpreter] Checking {} toggle bindings, field_name={}, completed_hold={:?}",
                                                    toggle_bindings.len(), field_name, completed_hold);
                                                if let Some(ref completed_hold_id) = completed_hold {
                                                    for binding in &toggle_bindings {
                                                        zoon::println!("[DD Interpreter] Toggle binding check: path={:?} vs field_name={}",
                                                            binding.event_path, field_name);
                                                        if binding.event_path.len() >= 2 && binding.event_path[0] == field_name.as_ref() {
                                                            if let Some(DdValue::LinkRef(link_id)) = inner_fields.get(binding.event_path[1].as_str()) {
                                                                // Found matching LinkRef - register BoolToggle with template's completed_hold
                                                                zoon::println!("[DD Interpreter] Template action: {} -> BoolToggle({}) (via parsed toggle binding)", link_id, completed_hold_id);
                                                                add_dynamic_link_action(link_id.to_string(), DynamicLinkAction::BoolToggle(completed_hold_id.clone()));
                                                                // Only register once per template (first matching binding is enough)
                                                                break;
                                                            }
                                                        }
                                                    }
                                                }
                                                break;
                                            }
                                        }
                                    }
                                    data_template = Some(result);
                                    data_func_name = Some(name.clone());
                                    break;
                                }
                            }
                        }
                    }
            }

            // IMPORTANT: Use the element_template from FilteredMappedListWithPredicate in the document.
            // This template was created by the evaluator with proper PlaceholderWhileRef structures
            // that get resolved to each item's HoldRefs during cloning.
            // Do NOT create element_template by calling functions with concrete data, as that
            // creates templates with concrete WhileRef { hold_id: "hold_X" } that can't be remapped.
            let element_template = extract_element_template_from_document(&document);

            if element_template.is_some() {
                zoon::println!("[DD Interpreter] Using element template from FilteredMappedListWithPredicate (has PlaceholderWhileRef)");
            } else if data_template.is_some() {
                zoon::println!("[DD Interpreter] WARNING: No element template found in document, list items may not render correctly");
            }

            if let Some(ref elem) = element_template {
                zoon::println!("[DD Interpreter] Created element template: {:?}", elem);
                // Initialize list_elements HOLD for dynamic item rendering
                update_hold_state_no_persist("list_elements", DdValue::List(std::sync::Arc::new(Vec::new())));
            }

            // Reconstruct persisted items that lost their HoldRef structure
            // Persisted items have empty elements Object, while fresh items have LinkRefs
            let initial_list = if let (Some(data_tmpl), DdValue::List(items)) = (&data_template, &initial_list) {
                let mut reconstructed_items = Vec::new();
                let mut reconstructed_elements = Vec::new();
                let mut any_reconstructed = false;

                for item in items.iter() {
                    // Check if this item needs reconstruction (empty elements field)
                    // Find the elements field dynamically (Object containing LinkRefs in template)
                    let needs_reconstruction = if let DdValue::Object(obj) = item {
                        // Check if any Object field is empty (elements were stripped during persistence)
                        obj.values().any(|v| {
                            matches!(v, DdValue::Object(inner) if inner.is_empty())
                        })
                    } else {
                        false
                    };

                    if needs_reconstruction {
                        zoon::println!("[DD Interpreter] Reconstructing persisted item: {:?}", item);
                        if let Some((new_data, new_elem)) = reconstruct_persisted_item(item, data_tmpl, element_template.as_ref()) {
                            reconstructed_items.push(new_data);
                            if let Some(elem) = new_elem {
                                reconstructed_elements.push(elem);
                            }
                            any_reconstructed = true;
                        } else {
                            // Failed to reconstruct - keep original (won't render title but count will be correct)
                            zoon::println!("[DD Interpreter] WARNING: Failed to reconstruct item, keeping original");
                            reconstructed_items.push(item.clone());
                        }
                    } else {
                        // Fresh item from static_list - needs unique IDs to avoid shared hover state bug
                        // Without this, all items share the same hover_link_XX hold and hovering one
                        // shows delete button on ALL items
                        zoon::println!("[DD Interpreter] Instantiating fresh item with unique IDs: {:?}", item);
                        if let Some((new_data, new_elem)) = instantiate_fresh_item(item, element_template.as_ref()) {
                            reconstructed_items.push(new_data);
                            if let Some(elem) = new_elem {
                                reconstructed_elements.push(elem);
                            }
                            any_reconstructed = true;  // We did modify the items
                        } else {
                            // Fallback - shouldn't happen for well-formed items
                            zoon::println!("[DD Interpreter] WARNING: Failed to instantiate fresh item, keeping original");
                            reconstructed_items.push(item.clone());
                        }
                    }
                }

                if any_reconstructed {
                    zoon::println!("[DD Interpreter] Reconstructed {} items, {} elements",
                        reconstructed_items.len(), reconstructed_elements.len());
                    // Update list_elements HOLD with reconstructed elements
                    if !reconstructed_elements.is_empty() {
                        update_hold_state_no_persist("list_elements", DdValue::List(std::sync::Arc::new(reconstructed_elements)));
                    }
                    DdValue::List(std::sync::Arc::new(reconstructed_items))
                } else {
                    initial_list.clone()
                }
            } else {
                initial_list.clone()
            };

            // Update HOLD_STATES with reconstructed list
            update_hold_state_no_persist(list_name, initial_list.clone());

            // Add list-append HOLDs manually (can't use add_list_append_on_enter which returns new Self)
            let transform = if let Some(ref data_tmpl) = data_template {
                // Discover the text HoldRef field name from the template
                let text_hold_field = if let DdValue::Object(fields) = &data_tmpl {
                    fields.iter()
                        .find(|(_, v)| {
                            if let DdValue::HoldRef(hold_id) = v {
                                if let Some(initial) = super::io::get_hold_value(hold_id) {
                                    return matches!(initial, DdValue::Text(_));
                                }
                            }
                            false
                        })
                        .map(|(k, _)| k.to_string())
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                zoon::println!("[DD Interpreter] Discovered text hold field: {:?}", text_hold_field);
                // Use template-based append for proper Element AST items
                StateTransform::ListAppendWithTemplate {
                    data_template: data_tmpl.clone(),
                    element_template,
                    title_hold_field: text_hold_field,
                }
            } else {
                // Fall back to simple object append
                zoon::println!("[DD Interpreter] WARNING: new_list_item function not found, using legacy ListAppend");
                StateTransform::ListAppend
            };

            config.holds.push(HoldConfig {
                id: HoldId::new(list_name),
                initial: initial_list.clone(),
                triggered_by: vec![LinkId::new(link_id)],
                timer_interval_ms: 0,
                filter: EventFilter::TextStartsWith("Enter:".to_string()),
                transform,
                persist: true,
            });
            // Task 7.1: Use dynamic text-clear HOLD ID (same as initialized above)
            config.holds.push(HoldConfig {
                id: HoldId::new(&text_clear_hold_id),
                initial: DdValue::text(""),
                triggered_by: vec![LinkId::new(link_id)],
                timer_interval_ms: 0,
                filter: EventFilter::TextStartsWith("Enter:".to_string()),
                transform: StateTransform::ClearText,
                persist: false,
            });
            // REMOVED: ToggleListItemCompleted - checkboxes use toggle_hold_bool() directly
            // REMOVED: SetListItemEditing - editing uses DynamicLinkAction::SetTrue directly
            // REMOVED: UpdateListItemTitle - saving uses DynamicLinkAction::EditingHandler directly

            // Remove HOLD for dynamic list item deletion (uses stable LinkRef identity)
            // Event format: "remove:LINK_ID" where LINK_ID is the remove button's LinkRef
            config.holds.push(HoldConfig {
                id: HoldId::new(list_name),
                initial: initial_list.clone(),
                triggered_by: vec![LinkId::new("dynamic_list_remove")],
                timer_interval_ms: 0,
                filter: EventFilter::TextStartsWith("remove:".to_string()),
                transform: StateTransform::RemoveListItem,
                persist: true,
            });
            // Note: Toggle-all is handled via HOLD body subscriptions in Boon code,
            // not through a StateTransform (see todo_mvc.bn lines 117-118)

            // Add Clear Completed HOLD if bulk remove event path was parsed from List/remove
            // This uses PARSED CODE STRUCTURE, not UI label matching!
            // The bulk_remove_event_path is set from: List/remove(item, on: elements.X.event.press |> THEN {...})
            let bulk_remove_path = get_bulk_remove_event_path();
            if !bulk_remove_path.is_empty() {
                zoon::println!("[DD Interpreter] Found bulk remove path from parsed code: {:?}", bulk_remove_path);
                // Resolve the path to get the actual LinkRef ID from the runtime
                if let Some(clear_completed_id) = resolve_path_to_link_ref(&runtime, &bulk_remove_path) {
                    zoon::println!("[DD Interpreter] Adding clear-completed for list: button_link={}", clear_completed_id);
                    config.holds.push(HoldConfig {
                        id: HoldId::new(list_name),
                        initial: initial_list,
                        triggered_by: vec![LinkId::new(&clear_completed_id)],
                        timer_interval_ms: 0,
                        filter: EventFilter::Any,
                        transform: StateTransform::ListRemoveCompleted,
                        persist: true,
                    });
                } else {
                    zoon::println!("[DD Interpreter] WARNING: Could not resolve bulk remove path {:?}", bulk_remove_path);
                }
            }

            // Register global toggle bindings (toggle-all checkbox)
            // These are extracted from HOLD bodies (like completed HOLD in each item) that contain:
            //   store.elements.toggle_all_checkbox.event.click |> THEN { store.all_completed |> Bool/not() }
            // We only need to register ONCE per unique LinkRef
            let global_toggle_bindings = get_global_toggle_bindings();
            zoon::println!("[DD Interpreter] Found {} global toggle bindings", global_toggle_bindings.len());
            let mut registered_toggle_links: std::collections::HashSet<String> = std::collections::HashSet::new();

            // Task 7.2: Dynamically detect the completed field name from template instead of hardcoding
            let completed_field_name = data_template.as_ref()
                .and_then(|tmpl| find_boolean_field_in_template(tmpl))
                .unwrap_or_else(|| "completed".to_string()); // Fallback for legacy compatibility
            zoon::println!("[DD Interpreter] Detected boolean field name: {}", completed_field_name);

            for binding in &global_toggle_bindings {
                zoon::println!("[DD Interpreter] Global toggle binding: event_path={:?}", binding.event_path);
                // Resolve the event_path to get the actual LinkRef ID from the runtime
                // Path like ["store", "elements", "toggle_all_checkbox"] -> find the LinkRef
                if let Some(link_id) = resolve_path_to_link_ref(&runtime, &binding.event_path) {
                    // Only register once per LinkRef
                    if registered_toggle_links.contains(&link_id) {
                        zoon::println!("[DD Interpreter] Skipping duplicate toggle-all LinkRef: {}", link_id);
                        continue;
                    }
                    registered_toggle_links.insert(link_id.clone());
                    use super::io::{add_dynamic_link_action, DynamicLinkAction};
                    // Use the detected list HOLD name (e.g., "todos"), not the individual completed HOLD ID
                    zoon::println!("[DD Interpreter] Registering toggle-all action: LinkRef={} for list {}, field={}", link_id, list_name, completed_field_name);
                    add_dynamic_link_action(link_id, DynamicLinkAction::ListToggleAllCompleted {
                        list_hold_id: list_name.clone(),  // Use the list HOLD, not individual item HOLD
                        completed_field: completed_field_name.clone(), // Task 7.2: Dynamic field name
                    });
                } else {
                    zoon::println!("[DD Interpreter] WARNING: Could not resolve toggle-all path {:?}", binding.event_path);
                }
            }
        }
        config
    } else if let (Some(link_id), Some(list_name)) = (&key_down_link, &list_hold_name) {
        // Task 7.1: Use detected list_name instead of hardcoded "items"
        // Text input with key_down pattern (shopping_list): key_down |> WHEN { Enter => append }
        // Use persisted value, or fall back to static evaluation, or empty list
        // FIX: Resolve HoldRef to its actual value - DD transform expects DdValue::List, not HoldRef
        let initial_list = load_persisted_hold_value(list_name)
            .unwrap_or_else(|| {
                match &static_items {
                    Some(DdValue::List(_)) => static_items.clone().unwrap(),
                    // HoldRef: Look up the actual list value from HOLD_STATES
                    Some(DdValue::HoldRef(hold_id)) => {
                        super::io::get_hold_value(hold_id.as_ref())
                            .unwrap_or_else(|| DdValue::List(std::sync::Arc::new(Vec::new())))
                    }
                    _ => DdValue::List(std::sync::Arc::new(Vec::new())),
                }
            });
        // Initialize HOLD_STATES so the bridge can read the initial value for reactive labels
        update_hold_state_no_persist(list_name, initial_list.clone());
        // Task 7.1: Use dynamic text-clear HOLD ID (derived from link ID)
        let text_clear_hold_id = format!("text_clear_{}", link_id);
        super::io::add_text_clear_hold(text_clear_hold_id.clone());
        update_hold_state_no_persist(&text_clear_hold_id, DdValue::text(""));

        // Check if there's also a clear button (List/clear pattern)
        if let Some(ref clear_link_id) = button_press_link {
            zoon::println!("[DD Interpreter] List-append-with-clear config: key_link={}, clear_link={}, text_clear={}, initial {:?}",
                link_id, clear_link_id, text_clear_hold_id, initial_list);
            DataflowConfig::new().add_list_append_with_clear(list_name, initial_list, link_id, clear_link_id, &text_clear_hold_id)
        } else {
            zoon::println!("[DD Interpreter] List-append config: link={}, text_clear={}, initial {:?}", link_id, text_clear_hold_id, initial_list);
            DataflowConfig::new().add_list_append_on_enter(list_name, initial_list, link_id, &text_clear_hold_id)
        }
    } else {
        // Link-driven pattern: button |> THEN |> HOLD/LATEST
        // Task 7.1: Use evaluator-built config with dynamic trigger IDs (no hardcoded fallback)
        // The evaluator populates triggered_by from extract_link_trigger_id()
        let has_evaluator_counter_holds = evaluator_config.holds.iter()
            .any(|h| !h.triggered_by.is_empty() && h.timer_interval_ms == 0);

        if has_evaluator_counter_holds {
            zoon::println!("[DD Interpreter] Using evaluator-built config for counter pattern ({} holds)", evaluator_config.holds.len());
            // Initialize HOLD_STATES from evaluator config
            for hold_config in &evaluator_config.holds {
                let hold_id = hold_config.id.name();
                let initial_value = load_persisted_hold_value(hold_id)
                    .unwrap_or_else(|| hold_config.initial.clone());
                update_hold_state_no_persist(hold_id, initial_value);
                zoon::println!("[DD Interpreter] Initialized HOLD {} with triggered_by: {:?}",
                    hold_id, hold_config.triggered_by);
            }
            evaluator_config
        } else {
            // Fallback for patterns not yet handled by evaluator
            zoon::println!("[DD Interpreter] WARNING: No evaluator config, using legacy fallback");
            let hold_id = "hold_0";
            let link_id = "link_1";
            let initial_value = load_persisted_hold_value(hold_id).unwrap_or_else(|| DdValue::int(0));
            update_hold_state_no_persist(hold_id, initial_value.clone());
            zoon::println!("[DD Interpreter] Legacy counter config: {} triggered by {}, initial {:?}", hold_id, link_id, initial_value);
            DataflowConfig::counter_with_initial_hold(link_id, hold_id, initial_value)
        }
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
            // Task 7.1: Clear text input DOM when any registered text-clear HOLD is updated
            // This implements the Boon pattern: text_to_add |> THEN { Text/empty() }
            let should_clear_text = update.hold_state_updates.keys()
                .any(|hold_id| super::io::is_text_clear_hold(hold_id));
            if should_clear_text {
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

/// Detect the list variable from the runtime by searching for List values.
///
/// Returns (list_value, variable_name) if found.
/// Checks common patterns in order: store.*, then top-level variables.
fn detect_list_variable(runtime: &BoonDdRuntime) -> (Option<DdValue>, Option<String>) {
    // First check if there's a "store" object with list fields
    if let Some(store) = runtime.get_variable("store") {
        if let DdValue::Object(fields) = store {
            // Look for any field that contains a List value or HoldRef to a list
            for (field_name, value) in fields.iter() {
                if matches!(value, DdValue::List(_)) {
                    return (Some(value.clone()), Some(field_name.to_string()));
                }
                // Also check for HoldRef - this is a reactive list like store.todos in todo_mvc
                if let DdValue::HoldRef(hold_id) = value {
                    // Return the HoldRef and the hold_id as the list name
                    zoon::println!("[DD Interpreter] Found HoldRef in store.{}: {}", field_name, hold_id);
                    return (Some(value.clone()), Some(hold_id.to_string()));
                }
            }
        }
    }

    // Task 7.1: Removed hardcoded priority search for ["items", "list", "data"]
    // Now searches ALL top-level variables for List values (generic)
    for (name, value) in runtime.get_all_variables() {
        if matches!(value, DdValue::List(_)) {
            return (Some(value.clone()), Some(name.clone()));
        }
    }

    (None, None)
}

/// Task 7.2: Find the field name for a boolean HoldRef in a template.
/// Given a template object and a HOLD ID, returns the field name that contains that HoldRef.
/// Used to dynamically determine "completed" field name instead of hardcoding.
fn find_boolean_field_in_template(template: &DdValue) -> Option<String> {
    match template {
        DdValue::Object(fields) => {
            // Look for a field with a boolean initial value (HoldRef pointing to bool HOLD)
            for (field_name, value) in fields.iter() {
                if let DdValue::HoldRef(hold_id) = value {
                    // Check if this HOLD has a boolean initial value
                    if let Some(hold_value) = super::io::get_hold_value(hold_id.as_ref()) {
                        if matches!(hold_value, DdValue::Bool(_)) {
                            // This is likely the "completed" field - return its name
                            zoon::println!("[DD Interpreter] Found boolean field: {} -> {}", field_name, hold_id);
                            return Some(field_name.to_string());
                        }
                    }
                }
            }
            None
        }
        DdValue::Tagged { fields, .. } => {
            // Same logic for Tagged objects
            for (field_name, value) in fields.iter() {
                if let DdValue::HoldRef(hold_id) = value {
                    if let Some(hold_value) = super::io::get_hold_value(hold_id.as_ref()) {
                        if matches!(hold_value, DdValue::Bool(_)) {
                            zoon::println!("[DD Interpreter] Found boolean field: {} -> {}", field_name, hold_id);
                            return Some(field_name.to_string());
                        }
                    }
                }
            }
            None
        }
        _ => None,
    }
}

// Task 6.3: extract_timer_info DELETED - evaluator builds timer config directly
// Task 6.3: extract_text_input_key_down DELETED - evaluator provides via set_text_input_key_down_link()

/// Information about a checkbox toggle pattern.
/// Used for list_example style patterns where checkbox clicks toggle boolean HOLDs.
struct CheckboxToggle {
    link_id: String,
    hold_id: String,
    initial: bool,
}

// Task 6.3: extract_checkbox_toggles DELETED - evaluator provides toggle bindings directly
// Task 6.3: has_filtered_mapped_list DELETED - evaluator provides via set_has_template_list()

/// Resolve a path like ["store", "elements", "toggle_all_checkbox"] to its LinkRef ID.
/// Traverses the runtime variables to find the LinkRef at the end of the path.
fn resolve_path_to_link_ref(runtime: &BoonDdRuntime, path: &[String]) -> Option<String> {
    if path.is_empty() {
        return None;
    }

    // Helper to traverse a path from a starting value
    fn traverse_path(start: &DdValue, path: &[String]) -> Option<String> {
        let mut current = start.clone();
        for segment in path {
            match &current {
                DdValue::Object(fields) => {
                    current = fields.get(segment.as_str())?.clone();
                }
                DdValue::Tagged { fields, .. } => {
                    current = fields.get(segment.as_str())?.clone();
                }
                _ => return None,
            }
        }
        // The final value should be a LinkRef
        if let DdValue::LinkRef(link_id) = current {
            Some(link_id.to_string())
        } else {
            None
        }
    }

    // First try: direct path lookup (e.g., "store" -> "elements" -> "button")
    if let Some(start) = runtime.get_variable(&path[0]) {
        if let Some(result) = traverse_path(start, &path[1..].to_vec()) {
            return Some(result);
        }
    }

    // Second try: if first segment isn't a variable, check if it's a field inside "store"
    // This handles relative paths like ["elements", "button"] which mean ["store", "elements", "button"]
    if let Some(store) = runtime.get_variable("store") {
        if let Some(result) = traverse_path(store, path) {
            zoon::println!("[DD resolve_path] Resolved via store prefix: {:?} -> {}", path, result);
            return Some(result);
        }
    }

    None
}

// Task 6.3: has_filtered_mapped_list_in_value DELETED - evaluator provides via set_has_template_list()

/// Extract the element_template from FilteredMappedListWithPredicate in the document.
/// This returns the template that was created by the evaluator with proper PlaceholderWhileRef structures.
fn extract_element_template_from_document(document: &Option<DdValue>) -> Option<DdValue> {
    let doc = document.as_ref()?;
    extract_element_template_from_value(doc)
}

/// Recursively search for and extract element_template from FilteredMappedListWithPredicate.
fn extract_element_template_from_value(value: &DdValue) -> Option<DdValue> {
    match value {
        DdValue::FilteredMappedListWithPredicate { element_template, .. } => {
            Some(element_template.as_ref().clone())
        }
        DdValue::FilteredMappedListRef { element_template, .. } => {
            Some(element_template.as_ref().clone())
        }
        DdValue::MappedListRef { element_template, .. } => {
            Some(element_template.as_ref().clone())
        }
        DdValue::Object(fields) => {
            for field_value in fields.values() {
                if let Some(template) = extract_element_template_from_value(field_value) {
                    return Some(template);
                }
            }
            None
        }
        DdValue::List(items) => {
            for item in items.iter() {
                if let Some(template) = extract_element_template_from_value(item) {
                    return Some(template);
                }
            }
            None
        }
        DdValue::Tagged { fields, .. } => {
            for field_value in fields.values() {
                if let Some(template) = extract_element_template_from_value(field_value) {
                    return Some(template);
                }
            }
            None
        }
        DdValue::WhileRef { arms, default, .. } => {
            for (_, body) in arms.iter() {
                if let Some(template) = extract_element_template_from_value(body) {
                    return Some(template);
                }
            }
            if let Some(d) = default {
                if let Some(template) = extract_element_template_from_value(d) {
                    return Some(template);
                }
            }
            None
        }
        _ => None,
    }
}

// Task 6.3: extract_checkbox_toggles_from_value DELETED - evaluator provides toggle bindings directly

/// Information about an editing toggle pattern.
/// Used for list_example style patterns where double-click enters editing mode.
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

// Task 6.3: extract_editing_toggles DELETED - evaluator provides editing bindings directly
// Note: toggle_all detection was removed - toggle-all is handled
// via HOLD body subscriptions in Boon code (see todo_mvc.bn lines 117-118)

// Task 6.3: extract_key_down_from_value DELETED - evaluator provides via set_text_input_key_down_link()
// Task 6.3: extract_button_press_link DELETED - evaluator provides via set_list_clear_link()
// Task 6.3: extract_timer_info_from_value DELETED - evaluator builds timer config directly

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
