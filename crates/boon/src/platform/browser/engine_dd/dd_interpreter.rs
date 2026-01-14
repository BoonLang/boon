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
use super::io::{EventInjector, set_global_dispatcher, clear_global_dispatcher, set_task_handle, clear_task_handle, set_output_listener_handle, clear_output_listener_handle, set_timer_handle, clear_timer_handle, clear_router_mappings, clear_dynamic_link_actions, update_hold_state, update_hold_state_no_persist, load_persisted_hold_value, set_checkbox_toggle_holds, clear_checkbox_toggle_holds, clear_hold_states_memory, set_list_var_name, clear_remove_event_path, clear_bulk_remove_event_path, get_editing_event_bindings, clear_editing_event_bindings, get_toggle_event_bindings, clear_toggle_event_bindings, get_global_toggle_bindings, clear_global_toggle_bindings, get_bulk_remove_event_path};
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

    // Get the initial list from static evaluation
    // Detect the list variable dynamically by looking for variables containing List values
    // Common patterns: store.items, store.list_data, items, list_data, or any variable containing a List
    let (static_list, list_var_name) = detect_list_variable(&runtime);
    zoon::println!("[DD Interpreter] Detected list variable: {:?}", list_var_name);
    // Store the detected name globally so bridge can use it for HOLD lookups
    let list_hold_name = list_var_name.clone().unwrap_or_else(|| "list_data".to_string());
    set_list_var_name(list_hold_name.clone());
    // Get the initial items list from static evaluation (for shopping_list)
    let static_items = runtime.get_variable("items").cloned();

    zoon::println!("[DD Interpreter] Evaluation complete, has document: {}",
        document.is_some());
    zoon::println!("[DD Interpreter] static_list = {:?}", static_list);
    zoon::println!("[DD Interpreter] static_items = {:?}", static_items);

    // Step 7: Set up DdWorker for reactive updates
    // Detect which pattern the code uses and configure accordingly
    let timer_info = extract_timer_info(&document);
    let key_down_link = extract_text_input_key_down(&document);
    let button_press_link = extract_button_press_link(&document);
    let checkbox_toggles = extract_checkbox_toggles(&document);
    let editing_toggles = extract_editing_toggles(&document);
    // NOTE: clear_completed_link is no longer extracted by label matching!
    // Instead, we use get_bulk_remove_event_path() which is set from parsed List/remove pattern

    let config = if let Some((ref hold_id, interval_ms)) = timer_info {
        // Timer-driven pattern: Duration |> Timer/interval() |> THEN |> Math/sum()
        // Timer values are NOT persisted - they're time-based, not user data
        // Start from 0 every time, don't pre-populate HOLD_STATES (so preview is empty until first tick)
        let initial_value = DdValue::int(0);
        zoon::println!("[DD Interpreter] Timer config: {} @ {}ms, initial {:?}", hold_id, interval_ms, initial_value);
        DataflowConfig::timer_counter(hold_id, initial_value, interval_ms)
    } else if !checkbox_toggles.is_empty() || has_filtered_mapped_list(&document) {
        // Checkbox toggle pattern (list_example) or template-based list (todo_mvc):
        // - list_example: checkbox.click |> THEN { state |> Bool/not() } - static checkboxes
        // - todo_mvc: FilteredMappedListWithPredicate - checkboxes inside templates use PlaceholderField
        // Each checkbox has its own HOLD for the completed state
        let has_template_list = has_filtered_mapped_list(&document);
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
        if let Some(ref link_id) = key_down_link {
            // Use persisted value, or fall back to in-memory HOLD state (set by eval_object), or empty list
            let initial_list = load_persisted_hold_value(&list_hold_name)
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
            update_hold_state_no_persist(&list_hold_name, initial_list.clone());
            // Initialize text_input_text HOLD for reactive text clearing
            update_hold_state_no_persist("text_input_text", DdValue::text(""));
            zoon::println!("[DD Interpreter] Also adding list-append for list: link={}", link_id);

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
            update_hold_state_no_persist(&list_hold_name, initial_list.clone());

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
                id: HoldId::new(&list_hold_name),
                initial: initial_list.clone(),
                triggered_by: vec![LinkId::new(link_id)],
                timer_interval_ms: 0,
                filter: EventFilter::TextStartsWith("Enter:".to_string()),
                transform,
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
            // REMOVED: ToggleListItemCompleted - checkboxes use toggle_hold_bool() directly
            // REMOVED: SetListItemEditing - editing uses DynamicLinkAction::SetTrue directly
            // REMOVED: UpdateListItemTitle - saving uses DynamicLinkAction::EditingHandler directly

            // Remove HOLD for dynamic list item deletion (uses stable LinkRef identity)
            // Event format: "remove:LINK_ID" where LINK_ID is the remove button's LinkRef
            config.holds.push(HoldConfig {
                id: HoldId::new(&list_hold_name),
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
                        id: HoldId::new(&list_hold_name),
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
                    zoon::println!("[DD Interpreter] Registering toggle-all action: LinkRef={} for list {}", link_id, list_hold_name);
                    add_dynamic_link_action(link_id, DynamicLinkAction::ListToggleAllCompleted {
                        list_hold_id: list_hold_name.clone(),  // Use the list HOLD, not individual item HOLD
                        completed_field: "completed".to_string(), // Standard field name
                    });
                } else {
                    zoon::println!("[DD Interpreter] WARNING: Could not resolve toggle-all path {:?}", binding.event_path);
                }
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

    // Then check top-level variables for List values
    // Check common names first, then any variable with a List
    let common_names = ["items", "list", "data"];
    for name in common_names {
        if let Some(value) = runtime.get_variable(name) {
            if matches!(value, DdValue::List(_)) {
                return (Some(value.clone()), Some(name.to_string()));
            }
        }
    }

    // If no common name found, search all variables for any List
    for (name, value) in runtime.get_all_variables() {
        if matches!(value, DdValue::List(_)) {
            return (Some(value.clone()), Some(name.clone()));
        }
    }

    (None, None)
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
/// Used for list_example style patterns where checkbox clicks toggle boolean HOLDs.
struct CheckboxToggle {
    link_id: String,
    hold_id: String,
    initial: bool,
}

/// Extract checkbox toggle patterns from the document.
///
/// Looks for list item objects with:
///   - item_elements.item_checkbox → LinkRef (click event trigger)
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

/// Check if the document contains FilteredMappedListWithPredicate (template-based list).
/// This indicates that the document uses template-based rendering like todo_mvc.
fn has_filtered_mapped_list(document: &Option<DdValue>) -> bool {
    let doc = match document.as_ref() {
        Some(d) => d,
        None => return false,
    };
    has_filtered_mapped_list_in_value(doc)
}

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

/// Recursively search for FilteredMappedListWithPredicate in the value.
fn has_filtered_mapped_list_in_value(value: &DdValue) -> bool {
    match value {
        DdValue::FilteredMappedListWithPredicate { .. } => true,
        DdValue::FilteredMappedListRef { .. } => true,
        DdValue::Object(fields) => {
            fields.values().any(has_filtered_mapped_list_in_value)
        }
        DdValue::List(items) => {
            items.iter().any(has_filtered_mapped_list_in_value)
        }
        DdValue::Tagged { fields, .. } => {
            fields.values().any(has_filtered_mapped_list_in_value)
        }
        DdValue::WhileRef { arms, default, .. } => {
            arms.iter().any(|(_, body)| has_filtered_mapped_list_in_value(body))
                || default.as_ref().map_or(false, |d| has_filtered_mapped_list_in_value(d))
        }
        _ => false,
    }
}

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
        DdValue::WhileRef { arms, .. } => {
            // Recurse into WhileRef arm bodies to find checkboxes
            // (checkboxes may be inside conditionally rendered content)
            for (_pattern, body) in arms.iter() {
                extract_checkbox_toggles_from_value(body, toggles);
            }
        }
        _ => {}
    }
}

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
// Note: toggle_all detection was removed - toggle-all is handled
// via HOLD body subscriptions in Boon code (see todo_mvc.bn lines 117-118)

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
        DdValue::WhileRef { arms, .. } => {
            // Recurse into WhileRef arm bodies
            for (_pattern, body) in arms.iter() {
                if let Some(id) = extract_key_down_from_value(body) {
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
        | DdValue::FilteredListRefWithPredicate { .. }
        | DdValue::ReactiveFilteredList { .. } | DdValue::ReactiveText { .. }
        | DdValue::Placeholder | DdValue::PlaceholderField { .. }
        | DdValue::PlaceholderWhileRef { .. } | DdValue::NegatedPlaceholderField { .. }
        | DdValue::MappedListRef { .. }
        | DdValue::FilteredMappedListRef { .. }
        | DdValue::FilteredMappedListWithPredicate { .. } => None,
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
