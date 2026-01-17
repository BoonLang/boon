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
//! These will be added in subsequent phases using Worker.

use chumsky::Parser as _;
use super::evaluator::{BoonDdRuntime, reset_cell_counter};
use super::super::core::value::Value;
use super::super::core::{Worker, DataflowConfig, CellConfig, CellId, LinkId, EventFilter, StateTransform, ElementTag, reconstruct_persisted_item, instantiate_fresh_item};
// Phase 7.3: Removed imports of deleted setter/clear functions (now in DataflowConfig)
use super::super::io::{
    EventInjector, set_global_dispatcher, clear_global_dispatcher,
    set_task_handle, clear_task_handle, clear_output_listener_handle,
    set_timer_handle, clear_timer_handle, clear_dynamic_link_actions,
    init_cell, load_persisted_cell_value, clear_cells_memory,
    // Getters only - setters removed (now via DataflowConfig)
    get_editing_event_bindings, get_toggle_event_bindings, get_global_toggle_bindings,
    get_bulk_remove_event_path,
    get_text_input_key_down_link, clear_text_input_key_down_link,
    get_list_clear_link, clear_list_clear_link,
    get_has_template_list, clear_has_template_list,
};
// Phase 11a: clear_router_mappings was removed - routing goes through DD dataflow now
#[cfg(target_arch = "wasm32")]
use super::super::render::bridge::clear_dd_text_input_value;
use zoon::{Task, StreamExt};
use crate::parser::{
    Input, SourceCode, Spanned, Token, lexer, parser, reset_expression_depth,
    resolve_persistence, resolve_references, span_at, static_expression,
};

/// Result of running DD reactive evaluation.
#[derive(Clone)]
pub struct DdResult {
    /// The document value if evaluation succeeded
    pub document: Option<Value>,
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
/// Check if a Value contains CellRefs (indicating it uses item data).
/// Used to distinguish element templates that use item data (like todo_item)
/// from container elements that don't (like main_panel).
fn has_dynamic_holds(value: &Value) -> bool {
    match value {
        Value::CellRef(_) => true,
        Value::List(items) => items.iter().any(has_dynamic_holds),
        Value::Object(fields) => fields.values().any(has_dynamic_holds),
        Value::Tagged { tag, fields, .. } => {
            // Check for __while_config__ Tagged value (pure DD WHILE replacement)
            if tag.as_ref() == "__while_config__" {
                // Check arms and default for CellRefs
                let arms_have_holds = fields.get("arms")
                    .and_then(|v| match v {
                        Value::List(items) => Some(items.iter().any(|item| {
                            item.get("body").map_or(false, has_dynamic_holds)
                        })),
                        _ => None,
                    })
                    .unwrap_or(false);
                let default_has_holds = fields.get("default")
                    .map_or(false, has_dynamic_holds);
                arms_have_holds || default_has_holds
            } else {
                // Regular Tagged value
                fields.values().any(has_dynamic_holds)
            }
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
    // Phase 11a: clear_router_mappings() removed - routing goes through DD dataflow now
    clear_dynamic_link_actions();  // Clear dynamic link→hold mappings
    // Phase 7.3: Config clearing now happens via clear_active_config() in Worker
    // Removed: clear_remove_event_path, clear_bulk_remove_event_path,
    //          clear_editing_event_bindings, clear_toggle_event_bindings, clear_global_toggle_bindings
    // DELETED: clear_checkbox_toggle_holds() - registry was dead code (set but never read)
    super::super::io::clear_text_clear_cells();  // Task 7.1: Clear text-clear HOLD registry (no-op now)
    clear_text_input_key_down_link();  // Clear text_input key_down LinkRef
    clear_list_clear_link();  // Clear List/clear event LinkRef
    clear_has_template_list();  // Clear template list flag
    clear_cells_memory();  // Prevent state contamination between examples
    #[cfg(target_arch = "wasm32")]
    clear_dd_text_input_value();  // Clear text input state
    reset_cell_counter();

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
    // This config contains CellConfig entries added by eval_hold()
    let evaluator_config = runtime.take_config();
    zoon::println!("[DD Interpreter] Evaluator built {} CellConfig entries", evaluator_config.cells.len());
    for (i, cell) in evaluator_config.cells.iter().enumerate() {
        zoon::println!("[DD Interpreter]   [{}] id={}, transform={:?}, timer={}ms",
            i, cell.id.name(), cell.transform, cell.timer_interval_ms);
    }

    // Get the initial list from static evaluation
    // Detect the list variable dynamically by looking for variables containing List values
    // Common patterns: store.items, store.list_data, items, list_data, or any variable containing a List
    let (static_list, list_var_name) = detect_list_variable(&runtime);
    zoon::println!("[DD Interpreter] Detected list variable: {:?}", list_var_name);
    // DEAD CODE DELETED: set_list_var_name() - set but never read
    // Task 7.1: Use detected list variable instead of hardcoded "items"
    let static_items = static_list.clone();

    zoon::println!("[DD Interpreter] Evaluation complete, has document: {}",
        document.is_some());
    zoon::println!("[DD Interpreter] static_list = {:?}", static_list);
    zoon::println!("[DD Interpreter] static_items = {:?}", static_items);

    // Step 7: Set up Worker for reactive updates
    // Task 4.3: Prefer evaluator-built config over extract_* pattern detection

    // Check if evaluator built timer HOLDs (timer_interval_ms > 0)
    let has_timer_hold = evaluator_config.cells.iter().any(|h| h.timer_interval_ms > 0);

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
    zoon::println!("[DD Interpreter] Editing bindings from evaluator: cell_id={:?}, has_link_ids={}",
        editing_bindings.cell_id, has_evaluator_editing_bindings);

    // Task 6.3: Get timer info from evaluator-built config ONLY (no fallback)
    let timer_info: Option<(String, u64)> = evaluator_config.cells.iter()
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
        zoon::println!("[DD Interpreter] Using evaluator-built config for timer pattern ({} cells)", evaluator_config.cells.len());
        evaluator_config
    } else if has_evaluator_toggle_holds {
        // Task 4.3: Use evaluator-built config for toggle patterns
        // Populate triggered_by from toggle bindings which now have link_ids
        zoon::println!("[DD Interpreter] Using evaluator-built config for toggle pattern ({} cells)", evaluator_config.cells.len());
        let mut config = evaluator_config;

        // Populate triggered_by for each CellConfig from toggle bindings
        for cell_config in &mut config.cells {
            // Find toggle binding for this cell
            for binding in &toggle_bindings {
                if binding.cell_id == cell_config.id.name() {
                    if let Some(ref link_id) = binding.link_id {
                        zoon::println!("[DD Interpreter] Populating triggered_by for {}: {}",
                            cell_config.id.name(), link_id);
                        cell_config.triggered_by.push(LinkId::new(link_id));
                    }
                }
            }
        }

        // DELETED: checkbox_cell_ids collection and set_checkbox_toggle_holds() call
        // Registry was dead code (set but never read)

        // Task 4.3: Add editing bindings from evaluator (SetTrue/SetFalse for edit mode)
        if has_evaluator_editing_bindings {
            if let Some(ref editing_cell_id) = editing_bindings.cell_id {
                // SetTrue triggered by double_click
                if let Some(ref link_id) = editing_bindings.edit_trigger_link_id {
                    zoon::println!("[DD Interpreter] Adding SetTrue for editing: {} triggered by {}", editing_cell_id, link_id);
                    config.cells.push(CellConfig {
                        id: CellId::new(editing_cell_id),
                        initial: Value::Bool(false),
                        triggered_by: vec![LinkId::new(link_id)],
                        timer_interval_ms: 0,
                        filter: EventFilter::Any,
                        transform: StateTransform::SetTrue,
                        persist: false,
                    });
                    // Also register DynamicLinkAction for replication when items are cloned
                    use super::super::io::{add_dynamic_link_action, DynamicLinkAction};
                    add_dynamic_link_action(link_id.clone(), DynamicLinkAction::SetTrue(editing_cell_id.clone()));
                }
                // SetFalse triggered by key_down (Enter/Escape)
                if let Some(ref link_id) = editing_bindings.exit_key_link_id {
                    zoon::println!("[DD Interpreter] Adding SetFalse (Enter) for editing: {} triggered by {}", editing_cell_id, link_id);
                    config.cells.push(CellConfig {
                        id: CellId::new(editing_cell_id),
                        initial: Value::Bool(false),
                        triggered_by: vec![LinkId::new(link_id)],
                        timer_interval_ms: 0,
                        filter: EventFilter::TextEquals("Enter".to_string()),
                        transform: StateTransform::SetFalse,
                        persist: false,
                    });
                    config.cells.push(CellConfig {
                        id: CellId::new(editing_cell_id),
                        initial: Value::Bool(false),
                        triggered_by: vec![LinkId::new(link_id)],
                        timer_interval_ms: 0,
                        filter: EventFilter::TextEquals("Escape".to_string()),
                        transform: StateTransform::SetFalse,
                        persist: false,
                    });
                    // Also register DynamicLinkAction for replication when items are cloned
                    use super::super::io::{add_dynamic_link_action, DynamicLinkAction};
                    add_dynamic_link_action(link_id.clone(), DynamicLinkAction::SetFalseOnKeys {
                        cell_id: editing_cell_id.clone(),
                        keys: vec!["Enter".to_string(), "Escape".to_string()],
                    });
                }
                // Initialize the editing HOLD state
                init_cell(editing_cell_id, Value::Bool(false));
            }
        }

        // Also check for text input key_down pattern (todo_mvc has BOTH toggles AND add-item input)
        // This is the same logic as in the checkbox/template branch at line ~522
        if let (Some(link_id), Some(list_name)) = (&key_down_link, &list_var_name) {
            // Use persisted value, or fall back to in-memory HOLD state (set by eval_object), or empty list
            let initial_list = load_persisted_cell_value(list_name)
                .unwrap_or_else(|| {
                    match &static_list {
                        Some(Value::List(_)) => static_list.clone().unwrap(),
                        // CellRef: eval_object already stored initial value in CELL_STATES
                        Some(Value::CellRef(cell_id)) => {
                            super::super::io::get_cell_value(&cell_id.name())
                                .unwrap_or_else(|| Value::List(std::sync::Arc::new(Vec::new())))
                        }
                        _ => Value::List(std::sync::Arc::new(Vec::new())),
                    }
                });
            zoon::println!("[DD Interpreter] toggle+list initial_list: {:?}", initial_list);
            // Clone initial_list for later use (it will be moved into CellConfig)
            let initial_list_for_registration = initial_list.clone();
            // Initialize CELL_STATES for reactive rendering
            init_cell(list_name, initial_list.clone());
            // Task 7.1: Use dynamic text-clear HOLD ID (derived from link ID)
            let text_clear_cell_id = format!("text_clear_{}", link_id);
            // Phase 7.3: text_clear_cell now registered via DataflowConfig methods
            init_cell(&text_clear_cell_id, Value::text(""));
            zoon::println!("[DD Interpreter] Toggle branch also adding list-append: link={}, list={}, text_clear={}", link_id, list_name, text_clear_cell_id);

            // Try to detect data template function for proper object creation (like checkbox/template branch)
            // This allows todo_mvc to create proper todo objects with title/completed/editing fields
            let func_names: Vec<String> = runtime.get_function_names().into_iter().cloned().collect();
            let mut data_template: Option<Value> = None;
            let mut element_template: Option<Value> = None;

            // Helper to check if a template has ANY boolean field initialized to True
            // Templates with True fields are "completed" items, we prefer ones with all False
            let has_true_boolean_field = |fields: &std::collections::BTreeMap<std::sync::Arc<str>, Value>| -> bool {
                use super::super::core::types::BoolTag;
                for value in fields.values() {
                    if let Value::CellRef(cell_id) = value {
                        if let Some(initial) = super::super::io::get_cell_value(&cell_id.name()) {
                            // Check for Tagged boolean with True tag
                            if let Value::Tagged { tag, .. } = &initial {
                                if BoolTag::is_true(tag.as_ref()) {
                                    return true;
                                }
                            }
                        }
                    }
                }
                false
            };

            // Track the parameter count of the best template so far
            let mut best_template_param_count: Option<usize> = None;

            for name in &func_names {
                let params = runtime.get_function_parameters(name);
                let param_count = params.as_ref().map(|p| p.len()).unwrap_or(0);
                let first_param = params.and_then(|params| params.first().cloned());

                if let Some(first_param) = first_param {
                    if let Some(result) = runtime.call_function(name, &[(first_param.as_str(), Value::text("__TEMPLATE__"))]) {
                        if let Value::Object(fields) = &result {
                            let has_hold_refs = fields.values().any(|v| matches!(v, Value::CellRef(_)));
                            if has_hold_refs {
                                // Prefer templates:
                                // 1. With FEWER parameters (single-param functions have proper defaults)
                                // 2. WITHOUT True boolean fields (new item templates over completed item templates)
                                let this_has_true = has_true_boolean_field(fields.as_ref());
                                let current_has_true = data_template.as_ref().map(|dt| {
                                    if let Value::Object(dt_fields) = dt {
                                        has_true_boolean_field(dt_fields.as_ref())
                                    } else { false }
                                }).unwrap_or(true); // Treat no template as "has true" so we take first one

                                // Determine if this is a better template:
                                // - No current template
                                // - Fewer parameters (single-param functions like new_todo are preferred)
                                // - Same param count but this one has no True and current has True
                                let fewer_params = best_template_param_count.map(|c| param_count < c).unwrap_or(false);
                                let same_params_better_bool = best_template_param_count == Some(param_count)
                                    && !this_has_true && current_has_true;
                                let is_better_template = data_template.is_none()
                                    || fewer_params
                                    || same_params_better_bool;

                                if is_better_template {
                                    zoon::println!("[DD Interpreter] Toggle branch found data template function: {} (params: {})", name, param_count);
                                    data_template = Some(result.clone());
                                    best_template_param_count = Some(param_count);
                                }
                            }
                        }
                        if let Value::Tagged { tag, .. } = &result {
                            if ElementTag::is_element(tag.as_ref()) && element_template.is_none() {
                                zoon::println!("[DD Interpreter] Toggle branch found element template function: {}", name);
                                element_template = Some(result.clone());
                            }
                        }
                    }
                }
            }

            // Register EditingHandler for template (needed for action replication to dynamic items)
            if let Some(ref data_tmpl) = data_template {
                if let Value::Object(fields) = data_tmpl {
                    // Discover editing_cell, title_cell, completed_cell, and elements field from template
                    let mut editing_cell: Option<String> = None;
                    let mut title_cell: Option<String> = None;
                    let mut completed_cell: Option<String> = None;
                    let mut elements_field: Option<(&std::sync::Arc<str>, &std::collections::BTreeMap<std::sync::Arc<str>, Value>)> = None;

                    for (field_name, field_value) in fields.iter() {
                        match field_value {
                            Value::CellRef(cell_id) => {
                                if let Some(initial) = super::super::io::get_cell_value(&cell_id.name()) {
                                    match initial {
                                        Value::Bool(_) | Value::Tagged { .. } => {
                                            if field_name.contains("edit") {
                                                editing_cell = Some(cell_id.to_string());
                                            } else if field_name.contains("complet") {
                                                // "completed", "complete", etc.
                                                completed_cell = Some(cell_id.to_string());
                                            }
                                        }
                                        Value::Text(_) => {
                                            title_cell = Some(cell_id.to_string());
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            Value::Object(inner_fields) => {
                                let has_link_refs = inner_fields.values().any(|v| matches!(v, Value::LinkRef(_)));
                                if has_link_refs {
                                    elements_field = Some((field_name, inner_fields.as_ref()));
                                }
                            }
                            _ => {}
                        }
                    }

                    // Register RemoveListItem action for template
                    // (Do this BEFORE EditingHandler to preserve elements_field reference)
                    let remove_path = super::super::io::get_remove_event_path();
                    if remove_path.len() >= 2 {
                        if let Some((elem_field_name, elem_fields)) = &elements_field {
                            if remove_path[0] == elem_field_name.as_ref() {
                                if let Some(Value::LinkRef(link_id)) = elem_fields.get(remove_path[1].as_str()) {
                                    zoon::println!("[DD Interpreter] Toggle branch: Registering RemoveListItem for template: {}", link_id);
                                    add_dynamic_link_action(link_id.to_string(), DynamicLinkAction::RemoveListItem { link_id: link_id.to_string() });
                                }
                            }
                        }
                    }

                    // Register EditingHandler if we found both editing_cell and title_cell
                    if let (Some(edit_hold), Some(t_hold), Some((elem_field_name, elem_fields))) =
                        (&editing_cell, &title_cell, elements_field.clone())
                    {
                        // Find the editing_todo_title_element LinkRef
                        let editing_bindings = get_editing_event_bindings();
                        if editing_bindings.exit_key_path.len() >= 2 && editing_bindings.exit_key_path[0] == elem_field_name.as_ref() {
                            if let Some(Value::LinkRef(link_id)) = elem_fields.get(editing_bindings.exit_key_path[1].as_str()) {
                                zoon::println!("[DD Interpreter] Toggle branch: Registering EditingHandler for template: {} -> (edit={}, title={})",
                                    link_id, edit_hold, t_hold);
                                add_dynamic_link_action(link_id.to_string(), DynamicLinkAction::EditingHandler {
                                    editing_cell: edit_hold.clone(),
                                    title_cell: t_hold.clone(),
                                });
                            }
                        }
                    }

                    // Register BoolToggle for template's checkbox link (needed for action replication to dynamic items)
                    // This allows the worker's action replication to find and remap the BoolToggle action
                    if let (Some(comp_hold), Some((elem_field_name, elem_fields))) =
                        (&completed_cell, elements_field.clone())
                    {
                        let toggle_bindings = get_toggle_event_bindings();
                        for binding in &toggle_bindings {
                            // Check if this binding's path matches our elements field
                            if binding.event_path.len() >= 2 && binding.event_path[0] == elem_field_name.as_ref() {
                                if let Some(Value::LinkRef(link_id)) = elem_fields.get(binding.event_path[1].as_str()) {
                                    zoon::println!("[DD Interpreter] Toggle branch: Registering BoolToggle for template: {} -> {}",
                                        link_id, comp_hold);
                                    add_dynamic_link_action(link_id.to_string(), DynamicLinkAction::BoolToggle(comp_hold.clone()));
                                    // Only register once per template
                                    break;
                                }
                            }
                        }
                    }
                }
            }

            // Determine the transform based on whether we have templates
            let transform = if let Some(ref data_tmpl) = data_template {
                // Discover the text CellRef field name from the template
                let text_cell_field = if let Value::Object(fields) = data_tmpl {
                    fields.iter()
                        .find(|(_, v)| {
                            if let Value::CellRef(cell_id) = v {
                                if let Some(initial) = super::super::io::get_cell_value(&cell_id.name()) {
                                    return matches!(initial, Value::Text(_));
                                }
                            }
                            false
                        })
                        .map(|(k, _)| k.to_string())
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                zoon::println!("[DD Interpreter] Toggle branch using ListAppendWithTemplate, text_cell_field={}", text_cell_field);
                StateTransform::ListAppendWithTemplate {
                    data_template: data_tmpl.clone(),
                    element_template: element_template.clone(),
                    title_cell_field: text_cell_field,
                }
            } else {
                zoon::println!("[DD Interpreter] Toggle branch using simple ListAppend (no template found)");
                StateTransform::ListAppend
            };

            // Check if there's also a clear button (List/clear pattern)
            if let Some(ref clear_link_id) = button_press_link {
                // HOLD for the list items - triggered by both Enter key AND clear button
                // Use template-aware variant if we have templates
                let final_transform = if let Some(ref data_tmpl) = data_template {
                    zoon::println!("[DD Interpreter] Toggle branch using ListAppendWithTemplateAndClear (template + clear button)");
                    // Get title_cell_field from the transform we already built
                    let title_cell_field = if let Value::Object(fields) = data_tmpl {
                        fields.iter()
                            .find(|(_, v)| {
                                if let Value::CellRef(cell_id) = v {
                                    if let Some(initial) = super::super::io::get_cell_value(&cell_id.name()) {
                                        return matches!(initial, Value::Text(_));
                                    }
                                }
                                false
                            })
                            .map(|(k, _)| k.to_string())
                            .unwrap_or_default()
                    } else {
                        String::new()
                    };
                    StateTransform::ListAppendWithTemplateAndClear {
                        data_template: data_tmpl.clone(),
                        element_template: element_template.clone(),
                        title_cell_field,
                        clear_link_id: clear_link_id.to_string(),
                    }
                } else {
                    zoon::println!("[DD Interpreter] Toggle branch using ListAppendWithClear (no template)");
                    StateTransform::ListAppendWithClear(clear_link_id.to_string())
                };
                config.cells.push(CellConfig {
                    id: CellId::new(list_name),
                    initial: initial_list,
                    triggered_by: vec![LinkId::new(link_id), LinkId::new(clear_link_id)],
                    timer_interval_ms: 0,
                    filter: EventFilter::Any, // Accept both Enter: and Unit events
                    transform: final_transform,
                    persist: true,
                });
            } else {
                // Add list append HOLD config - triggered by Enter key from text input
                config.cells.push(CellConfig {
                    id: CellId::new(list_name),
                    initial: initial_list,
                    triggered_by: vec![LinkId::new(link_id)],
                    timer_interval_ms: 0,
                    filter: EventFilter::TextStartsWith("Enter:".to_string()),
                    transform,
                    persist: true,
                });
            }
            // Add text-clear HOLD config - same trigger, clears input on successful append
            config.cells.push(CellConfig {
                id: CellId::new(&text_clear_cell_id),
                initial: Value::text(""),
                triggered_by: vec![LinkId::new(link_id)],
                timer_interval_ms: 0,
                filter: EventFilter::TextStartsWith("Enter:".to_string()),
                transform: StateTransform::ClearText,
                persist: false,
            });

            // Add RemoveListItem HOLD config - listens to dynamic_list_remove events
            // Event format: "remove:LINK_ID" where LINK_ID is the remove button's LinkRef
            config.cells.push(CellConfig {
                id: CellId::new(list_name),
                initial: initial_list_for_registration.clone(),
                triggered_by: vec![LinkId::new("dynamic_list_remove")],
                timer_interval_ms: 0,
                filter: EventFilter::TextStartsWith("remove:".to_string()),
                transform: StateTransform::RemoveListItem,
                persist: true,
            });

            // Detect the completed field name from template FIRST
            // This is needed for both ListRemoveCompleted and toggle-all features
            // NO FALLBACKS: If template exists, field MUST be found explicitly (no silent "completed" default)
            let completed_field_name: Option<String> = match data_template.as_ref() {
                Some(tmpl) => {
                    let detected = find_boolean_field_in_template(tmpl);
                    if detected.is_none() {
                        zoon::println!("[DD Interpreter] WARNING: Could not detect boolean field in template. \
                            This means the template doesn't have a CellRef pointing to a boolean HOLD. \
                            Features requiring completion field (clear-completed, toggle-all) will be skipped.");
                    }
                    detected
                },
                None => {
                    zoon::println!("[DD Interpreter] WARNING: No data template found, skipping completion field detection");
                    None
                }
            };

            // Add ListRemoveCompleted HOLD config - bulk remove completed items (Clear completed button)
            // Only register if we have a valid completed_field_name (NO FALLBACKS)
            let bulk_remove_path = get_bulk_remove_event_path();
            zoon::println!("[DD Interpreter] toggle+list: bulk_remove_path = {:?}", bulk_remove_path);
            if !bulk_remove_path.is_empty() {
                if let Some(ref field_name) = completed_field_name {
                    zoon::println!("[DD Interpreter] toggle+list: Found bulk remove path from parsed code: {:?}", bulk_remove_path);
                    // Resolve the path to get the actual LinkRef ID from the runtime
                    if let Some(clear_completed_id) = resolve_path_to_link_ref(&runtime, &bulk_remove_path) {
                        zoon::println!("[DD Interpreter] toggle+list: Adding clear-completed for list: button_link={}, field={}", clear_completed_id, field_name);
                        config.cells.push(CellConfig {
                            id: CellId::new(list_name),
                            initial: initial_list_for_registration.clone(),
                            triggered_by: vec![LinkId::new(&clear_completed_id)],
                            timer_interval_ms: 0,
                            filter: EventFilter::Any,
                            transform: StateTransform::ListRemoveCompleted {
                                completed_field: field_name.clone(),
                            },
                            persist: true,
                        });
                    } else {
                        zoon::println!("[DD Interpreter] toggle+list: WARNING: Could not resolve bulk remove path {:?}", bulk_remove_path);
                    }
                } else {
                    zoon::println!("[DD Interpreter] toggle+list: Skipping clear-completed (no valid completion field detected)");
                }
            }

            // Register individual checkbox toggle bindings for toggle+list branch
            // These are extracted from HOLD bodies that contain:
            //   todo_elements.todo_checkbox.event.click |> THEN { completed |> Bool/not() }
            use super::super::io::{add_dynamic_link_action, DynamicLinkAction};
            zoon::println!("[DD Interpreter] toggle+list: Registering {} individual toggle bindings", toggle_bindings.len());
            for binding in &toggle_bindings {
                if let Some(ref checkbox_link_id) = binding.link_id {
                    zoon::println!("[DD Interpreter] toggle+list: Registering BoolToggle: {} -> {}",
                        checkbox_link_id, binding.cell_id);
                    add_dynamic_link_action(checkbox_link_id.clone(), DynamicLinkAction::BoolToggle(binding.cell_id.clone()));
                }
            }

            // Register global toggle bindings (toggle-all checkbox) for toggle+list branch
            // These are extracted from HOLD bodies that contain:
            //   store.elements.toggle_all_checkbox.event.click |> THEN { store.all_completed |> Bool/not() }
            let global_toggle_bindings = get_global_toggle_bindings();
            zoon::println!("[DD Interpreter] toggle+list: Found {} global toggle bindings", global_toggle_bindings.len());
            let mut registered_toggle_links: std::collections::HashSet<String> = std::collections::HashSet::new();

            // Only register toggle-all actions if we successfully detected the field name
            if let Some(ref field_name) = completed_field_name {
                zoon::println!("[DD Interpreter] toggle+list: Detected boolean field name: {}", field_name);
                for binding in &global_toggle_bindings {
                    zoon::println!("[DD Interpreter] toggle+list: Global toggle binding: event_path={:?}", binding.event_path);
                    // Resolve the event_path to get the actual LinkRef ID from the runtime
                    if let Some(resolved_link_id) = resolve_path_to_link_ref(&runtime, &binding.event_path) {
                        // Only register once per LinkRef
                        if registered_toggle_links.contains(&resolved_link_id) {
                            zoon::println!("[DD Interpreter] toggle+list: Skipping duplicate toggle-all LinkRef: {}", resolved_link_id);
                            continue;
                        }
                        registered_toggle_links.insert(resolved_link_id.clone());
                        zoon::println!("[DD Interpreter] toggle+list: Registering toggle-all action: LinkRef={} for list {}, field={}",
                            resolved_link_id, list_name, field_name);
                        add_dynamic_link_action(resolved_link_id, DynamicLinkAction::ListToggleAllCompleted {
                            list_cell_id: list_name.clone(),
                            completed_field: field_name.clone(),
                        });
                    } else {
                        zoon::println!("[DD Interpreter] toggle+list: WARNING: Could not resolve toggle-all path {:?}", binding.event_path);
                    }
                }
            } else {
                zoon::println!("[DD Interpreter] toggle+list: Skipping toggle-all registration (no valid field found)");
            }

            // Register RemoveListItem action for INITIAL ITEMS (existing persisted todos)
            // NOTE: In toggle+list branch, initial_list contains persisted items WITH LinkRefs
            // (LinkRefs are assigned when items are added, so persisted data does have them)
            let remove_path = super::super::io::get_remove_event_path();
            if let Value::List(items) = &initial_list_for_registration {
                for item in items.iter() {
                    if let Value::Object(obj) = item {
                        // Find the elements field dynamically (Object containing LinkRefs)
                        let elements_field_opt = obj.iter()
                            .find(|(_, v)| matches!(v, Value::Object(inner) if inner.values().any(|iv| matches!(iv, Value::LinkRef(_)))))
                            .map(|(k, v)| (k.clone(), v.clone()));

                        if let Some((elements_name, Value::Object(item_elements))) = elements_field_opt {
                            // Register RemoveListItem action using parsed path
                            if remove_path.len() >= 2 && remove_path[0] == elements_name.as_ref() {
                                if let Some(Value::LinkRef(link_id)) = item_elements.get(remove_path[1].as_str()) {
                                    zoon::println!("[DD Interpreter] toggle+list initial item: {} -> RemoveListItem", link_id);
                                    add_dynamic_link_action(link_id.to_string(), DynamicLinkAction::RemoveListItem { link_id: link_id.to_string() });
                                }
                            }
                        }
                    }
                }
            }
        }

        config
    } else if let Some((ref cell_id, interval_ms)) = timer_info {
        // Legacy: Timer-driven pattern detected via extract_timer_info
        // (This branch should no longer be reached for timer patterns)
        let initial_value = Value::int(0);
        zoon::println!("[DD Interpreter] Timer config: {} @ {}ms, initial {:?}", cell_id, interval_ms, initial_value);
        DataflowConfig::timer_counter(cell_id, initial_value, interval_ms)
    } else if !checkbox_toggles.is_empty() || get_has_template_list() {
        // Checkbox toggle pattern (list_example) or template-based list (todo_mvc):
        // - list_example: checkbox.click |> THEN { state |> Bool/not() } - static checkboxes
        // - todo_mvc: List mapping with __placeholder_field__ Tagged values for template checkboxes
        // Each checkbox has its own HOLD for the completed state
        // Task 6.3: Use evaluator-provided flag instead of document scanning
        let has_template_list = get_has_template_list();
        zoon::println!("[DD Interpreter] Checkbox/template config: {} toggles, has_template_list: {}", checkbox_toggles.len(), has_template_list);
        let mut config = DataflowConfig::new();
        for toggle in &checkbox_toggles {
            // Initialize CELL_STATES for reactive rendering
            init_cell(&toggle.cell_id, Value::Bool(toggle.initial));
            // Only trigger on own checkbox click
            // (toggle_all is handled via HOLD body subscriptions in the Boon code)
            let triggers = vec![LinkId::new(&toggle.link_id)];
            // Add BoolToggle HOLD config
            config.cells.push(CellConfig {
                id: CellId::new(&toggle.cell_id),
                initial: Value::Bool(toggle.initial),
                triggered_by: triggers,
                timer_interval_ms: 0,
                filter: EventFilter::Any,
                transform: StateTransform::BoolToggle,
                persist: true,
            });
        }
        // DELETED: checkbox_cell_ids and set_checkbox_toggle_holds() - registry was dead code

        // Add editing toggle HOLDs (for double-click to edit in list_example)
        for toggle in &editing_toggles {
            // Initialize editing HOLD to false (not editing initially)
            init_cell(&toggle.cell_id, Value::Bool(false));

            // Add SetTrue HOLD triggered by double_click
            config.cells.push(CellConfig {
                id: CellId::new(&toggle.cell_id),
                initial: Value::Bool(false),
                triggered_by: vec![LinkId::new(&toggle.double_click_link)],
                timer_interval_ms: 0,
                filter: EventFilter::Any,
                transform: StateTransform::SetTrue,
                persist: false, // Don't persist editing state
            });
            // Also register DynamicLinkAction for replication when items are cloned
            use super::super::io::{add_dynamic_link_action, DynamicLinkAction};
            add_dynamic_link_action(toggle.double_click_link.clone(), DynamicLinkAction::SetTrue(toggle.cell_id.clone()));

            // NOTE: Blur CellConfig is intentionally NOT added here because:
            // When inner events (change, key_down, blur) share the same LinkRef (link_53),
            // EventFilter::Any on blur would trigger on change events too, immediately
            // exiting edit mode. This is a known limitation until the interpreter creates
            // unique LinkRefs per event type. For now, rely on Enter/Escape to exit editing.
            // See: https://github.com/anthropics/boon/issues/XXX (TODO: file issue)
            let _ = &toggle.blur_link; // silence unused warning

            // Add SetFalse HOLD triggered by key_down with Enter or Escape (if present)
            if let Some(ref key_down_link) = toggle.key_down_link {
                // For Enter key
                config.cells.push(CellConfig {
                    id: CellId::new(&toggle.cell_id),
                    initial: Value::Bool(false),
                    triggered_by: vec![LinkId::new(key_down_link)],
                    timer_interval_ms: 0,
                    filter: EventFilter::TextEquals("Enter".to_string()),
                    transform: StateTransform::SetFalse,
                    persist: false,
                });
                // For Escape key
                config.cells.push(CellConfig {
                    id: CellId::new(&toggle.cell_id),
                    initial: Value::Bool(false),
                    triggered_by: vec![LinkId::new(key_down_link)],
                    timer_interval_ms: 0,
                    filter: EventFilter::TextEquals("Escape".to_string()),
                    transform: StateTransform::SetFalse,
                    persist: false,
                });
                // Also register DynamicLinkAction for replication when items are cloned
                use super::super::io::{add_dynamic_link_action, DynamicLinkAction};
                add_dynamic_link_action(key_down_link.clone(), DynamicLinkAction::SetFalseOnKeys {
                    cell_id: toggle.cell_id.clone(),
                    keys: vec!["Enter".to_string(), "Escape".to_string()],
                });
            }

            zoon::println!("[DD Interpreter] Added editing toggle config for hold {}", toggle.cell_id);
        }

        // Also check for text input key_down pattern (list_example has BOTH checkboxes AND add-item input)
        // Task 7.1: Only process if we have both a key_down link AND a detected list variable
        if let (Some(link_id), Some(list_name)) = (&key_down_link, &list_var_name) {
            // Use persisted value, or fall back to in-memory HOLD state (set by eval_object), or empty list
            let initial_list = load_persisted_cell_value(list_name)
                .unwrap_or_else(|| {
                    match &static_list {
                        Some(Value::List(_)) => static_list.clone().unwrap(),
                        // CellRef: eval_object already stored initial value in CELL_STATES
                        Some(Value::CellRef(cell_id)) => {
                            super::super::io::get_cell_value(&cell_id.name())
                                .unwrap_or_else(|| Value::List(std::sync::Arc::new(Vec::new())))
                        }
                        _ => Value::List(std::sync::Arc::new(Vec::new())),
                    }
                });
            zoon::println!("[DD Interpreter] list initial_list: {:?}", initial_list);
            // Initialize CELL_STATES for reactive rendering
            init_cell(list_name, initial_list.clone());
            // Initialize text-clear HOLD for reactive text clearing (Task 7.1: dynamic name from link ID)
            let text_clear_cell_id = format!("text_clear_{}", link_id);
            // Phase 7.3: text_clear_cell now registered via DataflowConfig methods
            init_cell(&text_clear_cell_id, Value::text(""));
            zoon::println!("[DD Interpreter] Also adding list-append for list: link={}, text_clear={}", link_id, text_clear_cell_id);

            // Register dynamic link actions for initial items
            // This allows the same mechanisms to work for both initial and dynamic items
            // NOTE: Use static_list (not initial_list) because persisted data doesn't include LinkRefs
            if let Some(Value::List(items)) = &static_list {
                use super::super::io::{add_dynamic_link_action, DynamicLinkAction};
                for item in items.iter() {
                    if let Value::Object(obj) = item {
                        // DEBUG: Log the item object structure
                        zoon::println!("[DD Interpreter] Initial item object fields: {:?}", obj.keys().collect::<Vec<_>>());

                        // Discover CellRef fields dynamically by type
                        // Find boolean CellRef (editing state) and text CellRef (title data)
                        let mut editing_cell: Option<String> = None;
                        let mut title_cell: Option<String> = None;
                        for (hold_field, hold_value) in obj.iter() {
                            if let Value::CellRef(cell_id) = hold_value {
                                // Check the initial value to determine type
                                if let Some(initial) = super::super::io::get_cell_value(&cell_id.name()) {
                                    match initial {
                                        Value::Bool(_) | Value::Tagged { .. } => {
                                            // Boolean or Tagged (True/False) - this is the editing state
                                            zoon::println!("[DD Interpreter] Initial item: detected boolean CellRef: {} = {}", hold_field, cell_id);
                                            editing_cell = Some(cell_id.to_string());
                                        }
                                        Value::Text(_) => {
                                            // Text - this is the title
                                            zoon::println!("[DD Interpreter] Initial item: detected text CellRef: {} = {}", hold_field, cell_id);
                                            title_cell = Some(cell_id.to_string());
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                        zoon::println!("[DD Interpreter] Discovered editing_cell={:?}, title_cell={:?}", editing_cell, title_cell);

                        // Find the elements field dynamically (Object containing LinkRefs)
                        let elements_field = obj.iter()
                            .find(|(_, v)| matches!(v, Value::Object(inner) if inner.values().any(|iv| matches!(iv, Value::LinkRef(_)))))
                            .map(|(k, v)| (k.clone(), v.clone()));

                        if let Some((elements_name, Value::Object(item_elements))) = elements_field {
                            zoon::println!("[DD Interpreter] Found elements field '{}' with {} LinkRefs", elements_name, item_elements.len());

                            // Register actions using PARSED PATH from List/remove(item, on: ...)
                            // Get the remove event path that was parsed from the Boon code
                            let remove_path = super::super::io::get_remove_event_path();
                            zoon::println!("[DD Interpreter] Using parsed remove path: {:?}", remove_path);

                            // Register remove action using parsed path (no pattern matching!)
                            if remove_path.len() >= 2 {
                                // Path is like ["todo_elements", "remove_todo_button"]
                                // First element is the field we're currently in (elements_name)
                                // Check if it matches and navigate to the LinkRef
                                if remove_path[0] == elements_name.as_ref() {
                                    if let Some(Value::LinkRef(link_id)) = item_elements.get(remove_path[1].as_str()) {
                                        zoon::println!("[DD Interpreter] Initial item: {} -> RemoveListItem (via parsed path)", link_id);
                                        add_dynamic_link_action(link_id.to_string(), DynamicLinkAction::RemoveListItem { link_id: link_id.to_string() });
                                    }
                                }
                            }

                            // Register editing actions using PARSED PATHS from HOLD body (no pattern matching!)
                            let editing_bindings = get_editing_event_bindings();
                            zoon::println!("[DD Interpreter] Using parsed editing bindings: edit_trigger={:?}, exit_key={:?}, exit_blur={:?}",
                                editing_bindings.edit_trigger_path, editing_bindings.exit_key_path, editing_bindings.exit_blur_path);

                            // Double-click element (edit_trigger_path) -> SetTrue(editing_cell)
                            if editing_bindings.edit_trigger_path.len() >= 2 && editing_bindings.edit_trigger_path[0] == elements_name.as_ref() {
                                if let Some(Value::LinkRef(link_id)) = item_elements.get(editing_bindings.edit_trigger_path[1].as_str()) {
                                    if let Some(ref edit_hold) = editing_cell {
                                        zoon::println!("[DD Interpreter] Initial item: {} -> SetTrue({}) (via parsed path)", link_id, edit_hold);
                                        add_dynamic_link_action(link_id.to_string(), DynamicLinkAction::SetTrue(edit_hold.clone()));
                                    }
                                }
                            }

                            // Key/Blur element (exit_key_path) -> EditingHandler + SetFalseOnKeys
                            if editing_bindings.exit_key_path.len() >= 2 && editing_bindings.exit_key_path[0] == elements_name.as_ref() {
                                if let Some(Value::LinkRef(link_id)) = item_elements.get(editing_bindings.exit_key_path[1].as_str()) {
                                    if let (Some(edit_hold), Some(t_hold)) = (&editing_cell, &title_cell) {
                                        zoon::println!("[DD Interpreter] Initial item: {} -> EditingHandler(edit={}, title={}) (via parsed path)", link_id, edit_hold, t_hold);
                                        add_dynamic_link_action(link_id.to_string(), DynamicLinkAction::EditingHandler {
                                            editing_cell: edit_hold.clone(),
                                            title_cell: t_hold.clone(),
                                        });
                                    }
                                    // Also register SetFalseOnKeys for each initial item (was only registered once globally)
                                    if let Some(ref edit_hold) = editing_cell {
                                        zoon::println!("[DD Interpreter] Initial item: {} -> SetFalseOnKeys({}) (via parsed path)", link_id, edit_hold);
                                        add_dynamic_link_action(link_id.to_string(), DynamicLinkAction::SetFalseOnKeys {
                                            cell_id: edit_hold.clone(),
                                            keys: vec!["Enter".to_string(), "Escape".to_string()],
                                        });
                                    }
                                }
                            }

                            // Register toggle bindings from HOLD body parsing (click |> THEN { state |> Bool/not() })
                            // Each binding has a specific cell_id that was created during item evaluation
                            let toggle_bindings = get_toggle_event_bindings();
                            for binding in &toggle_bindings {
                                // Check if the binding path matches this item's elements
                                if binding.event_path.len() >= 2 && binding.event_path[0] == elements_name.as_ref() {
                                    if let Some(Value::LinkRef(link_id)) = item_elements.get(binding.event_path[1].as_str()) {
                                        // Check if this item contains the binding's cell_id
                                        // The binding was created during this item's evaluation, so cell_id should match
                                        let item_has_this_hold = obj.values().any(|v| {
                                            matches!(v, Value::CellRef(id) if id.name() == binding.cell_id)
                                        });
                                        if item_has_this_hold {
                                            zoon::println!("[DD Interpreter] Initial item: {} -> BoolToggle({}) (via parsed toggle binding)", link_id, binding.cell_id);
                                            add_dynamic_link_action(link_id.to_string(), DynamicLinkAction::BoolToggle(binding.cell_id.clone()));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Try to create both templates by detecting available functions based on OUTPUT:
            // 1. data_template: A function that returns an Object with CellRef fields
            // 2. element_template: A function that returns a Tagged Element
            // This makes the engine truly generic - no assumptions about naming conventions
            let func_names: Vec<String> = runtime.get_function_names().into_iter().cloned().collect();

            // Helper to check if a template has ANY boolean field initialized to True
            let has_true_boolean_field_local = |fields: &std::collections::BTreeMap<std::sync::Arc<str>, Value>| -> bool {
                use super::super::core::types::BoolTag;
                for value in fields.values() {
                    if let Value::CellRef(cell_id) = value {
                        if let Some(initial) = super::super::io::get_cell_value(&cell_id.name()) {
                            if let Value::Tagged { tag, .. } = &initial {
                                if BoolTag::is_true(tag.as_ref()) {
                                    return true;
                                }
                            }
                        }
                    }
                }
                false
            };

            // Find data template function by testing each function's output
            // Prefer single-parameter functions (they have proper defaults built in)
            let mut data_template: Option<Value> = None;
            let mut data_func_name: Option<String> = None;
            let mut best_template_param_count: Option<usize> = None;

            for name in &func_names {
                // Get actual parameter names from the function definition
                // Clone the first param to release the borrow before calling call_function
                let params = runtime.get_function_parameters(name);
                let param_count = params.as_ref().map(|p| p.len()).unwrap_or(0);
                let first_param = params.and_then(|params| params.first().cloned());

                // Only try functions that take a parameter (data templates take an item parameter)
                if let Some(first_param) = first_param {
                        // Try calling with the actual parameter name
                        if let Some(result) = runtime.call_function(name, &[(first_param.as_str(), Value::text("__TEMPLATE__"))]) {
                            // Check if result is an Object with CellRef fields (indicates a list item template)
                            if let Value::Object(fields) = &result {
                                let has_hold_refs = fields.values().any(|v| matches!(v, Value::CellRef(_)));
                                if has_hold_refs {
                                    // Prefer templates with FEWER parameters (single-param functions have proper defaults)
                                    // and WITHOUT True boolean fields
                                    let this_has_true = has_true_boolean_field_local(fields.as_ref());
                                    let current_has_true = data_template.as_ref().map(|dt| {
                                        if let Value::Object(dt_fields) = dt {
                                            has_true_boolean_field_local(dt_fields.as_ref())
                                        } else { false }
                                    }).unwrap_or(true);

                                    let fewer_params = best_template_param_count.map(|c| param_count < c).unwrap_or(false);
                                    let same_params_better_bool = best_template_param_count == Some(param_count)
                                        && !this_has_true && current_has_true;
                                    let is_better_template = data_template.is_none()
                                        || fewer_params
                                        || same_params_better_bool;

                                    if !is_better_template {
                                        continue; // Skip this template, current one is better
                                    }

                                    zoon::println!("[DD Interpreter] Found data template function: {} (params: {}) -> {:?}", name, param_count, result);
                                    // Detect the elements field name (Object field containing LinkRefs)
                                    // and register actions for template's LinkRefs
                                    for (field_name, field_value) in fields.iter() {
                                        if let Value::Object(inner_fields) = field_value {
                                            let has_link_refs = inner_fields.values().any(|v| matches!(v, Value::LinkRef(_)));
                                            if has_link_refs {
                                                zoon::println!("[DD Interpreter] Detected elements field name: {}", field_name);
                                                // DEAD CODE DELETED: set_elements_field_name() - set but never read

                                                // Discover CellRef fields dynamically by type
                                                // Find boolean CellRefs (editing AND completed) and text CellRef (title)
                                                let mut editing_cell: Option<String> = None;
                                                let mut completed_cell: Option<String> = None;
                                                let mut title_cell: Option<String> = None;
                                                for (hold_field, hold_value) in fields.iter() {
                                                    if let Value::CellRef(cell_id) = hold_value {
                                                        // Check the initial value to determine type
                                                        if let Some(initial) = super::super::io::get_cell_value(&cell_id.name()) {
                                                            match initial {
                                                                Value::Bool(_) | Value::Tagged { .. } => {
                                                                    // Boolean or Tagged (True/False)
                                                                    // Distinguish by field name: "editing" vs "completed"
                                                                    if hold_field.contains("edit") {
                                                                        zoon::println!("[DD Interpreter] Detected editing CellRef: {} = {}", hold_field, cell_id);
                                                                        editing_cell = Some(cell_id.to_string());
                                                                    } else {
                                                                        zoon::println!("[DD Interpreter] Detected completed CellRef: {} = {}", hold_field, cell_id);
                                                                        completed_cell = Some(cell_id.to_string());
                                                                    }
                                                                }
                                                                Value::Text(_) => {
                                                                    // Text - this is the title
                                                                    zoon::println!("[DD Interpreter] Detected text CellRef: {} = {}", hold_field, cell_id);
                                                                    title_cell = Some(cell_id.to_string());
                                                                }
                                                                _ => {}
                                                            }
                                                        }
                                                    }
                                                }

                                                use super::super::io::{add_dynamic_link_action, DynamicLinkAction};

                                                // Use PARSED PATH from List/remove for remove action (no pattern matching!)
                                                let remove_path = super::super::io::get_remove_event_path();
                                                zoon::println!("[DD Interpreter] Template using parsed remove path: {:?}", remove_path);

                                                // Register remove action using parsed path
                                                if remove_path.len() >= 2 && remove_path[0] == field_name.as_ref() {
                                                    if let Some(Value::LinkRef(link_id)) = inner_fields.get(remove_path[1].as_str()) {
                                                        zoon::println!("[DD Interpreter] Template action: {} -> RemoveListItem (via parsed path)", link_id);
                                                        add_dynamic_link_action(link_id.to_string(), DynamicLinkAction::RemoveListItem { link_id: link_id.to_string() });
                                                    }
                                                }

                                                // Register editing actions using PARSED PATHS from HOLD body (no pattern matching!)
                                                let editing_bindings = get_editing_event_bindings();

                                                // Double-click element (edit_trigger_path) -> SetTrue(editing_cell)
                                                if editing_bindings.edit_trigger_path.len() >= 2 && editing_bindings.edit_trigger_path[0] == field_name.as_ref() {
                                                    if let Some(Value::LinkRef(link_id)) = inner_fields.get(editing_bindings.edit_trigger_path[1].as_str()) {
                                                        if let Some(ref edit_hold) = editing_cell {
                                                            zoon::println!("[DD Interpreter] Template action: {} -> SetTrue({}) (via parsed path)", link_id, edit_hold);
                                                            add_dynamic_link_action(link_id.to_string(), DynamicLinkAction::SetTrue(edit_hold.clone()));
                                                        }
                                                    }
                                                }

                                                // Key/Blur element (exit_key_path) -> EditingHandler + SetFalseOnKeys
                                                if editing_bindings.exit_key_path.len() >= 2 && editing_bindings.exit_key_path[0] == field_name.as_ref() {
                                                    if let Some(Value::LinkRef(link_id)) = inner_fields.get(editing_bindings.exit_key_path[1].as_str()) {
                                                        if let (Some(edit_hold), Some(t_hold)) = (&editing_cell, &title_cell) {
                                                            zoon::println!("[DD Interpreter] Template action: {} -> EditingHandler(edit={}, title={}) (via parsed path)", link_id, edit_hold, t_hold);
                                                            add_dynamic_link_action(link_id.to_string(), DynamicLinkAction::EditingHandler {
                                                                editing_cell: edit_hold.clone(),
                                                                title_cell: t_hold.clone(),
                                                            });
                                                        }
                                                        // Also register SetFalseOnKeys for template (needed for cloning/replication)
                                                        if let Some(ref edit_hold) = editing_cell {
                                                            zoon::println!("[DD Interpreter] Template action: {} -> SetFalseOnKeys({}) (via parsed path)", link_id, edit_hold);
                                                            add_dynamic_link_action(link_id.to_string(), DynamicLinkAction::SetFalseOnKeys {
                                                                cell_id: edit_hold.clone(),
                                                                keys: vec!["Enter".to_string(), "Escape".to_string()],
                                                            });
                                                        }
                                                    }
                                                }

                                                // Register toggle bindings from HOLD body parsing
                                                // Toggle bindings indicate checkbox -> completed toggle patterns
                                                // Use the detected completed_cell (not the binding's cell_id which is from existing items)
                                                let toggle_bindings = get_toggle_event_bindings();
                                                zoon::println!("[DD Interpreter] Checking {} toggle bindings, field_name={}, completed_cell={:?}",
                                                    toggle_bindings.len(), field_name, completed_cell);
                                                if let Some(ref completed_cell_id) = completed_cell {
                                                    for binding in &toggle_bindings {
                                                        zoon::println!("[DD Interpreter] Toggle binding check: path={:?} vs field_name={}",
                                                            binding.event_path, field_name);
                                                        if binding.event_path.len() >= 2 && binding.event_path[0] == field_name.as_ref() {
                                                            if let Some(Value::LinkRef(link_id)) = inner_fields.get(binding.event_path[1].as_str()) {
                                                                // Found matching LinkRef - register BoolToggle with template's completed_cell
                                                                zoon::println!("[DD Interpreter] Template action: {} -> BoolToggle({}) (via parsed toggle binding)", link_id, completed_cell_id);
                                                                add_dynamic_link_action(link_id.to_string(), DynamicLinkAction::BoolToggle(completed_cell_id.clone()));
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
                                    best_template_param_count = Some(param_count);
                                    // Continue loop to find better templates (fewer params)
                                }
                            }
                        }
                    }
            }

            // IMPORTANT: Use the element_template from list mapping Tagged values in the document.
            // Pure DD: Templates now use __placeholder_while__ and __placeholder_field__ Tagged values
            // that get resolved to each item's CellRefs during cloning.
            // Do NOT create element_template by calling functions with concrete data, as that
            // creates templates with concrete __while_config__ that can't be remapped.
            let element_template = extract_element_template_from_document(&document);

            if element_template.is_some() {
                zoon::println!("[DD Interpreter] Using element template from list mapping (has __placeholder_while__ config)");
            } else if data_template.is_some() {
                zoon::println!("[DD Interpreter] WARNING: No element template found in document, list items may not render correctly");
            }

            if let Some(ref elem) = element_template {
                zoon::println!("[DD Interpreter] Created element template: {:?}", elem);
                // Initialize list_elements HOLD for dynamic item rendering
                init_cell("list_elements", Value::List(std::sync::Arc::new(Vec::new())));
            }

            // Reconstruct persisted items that lost their CellRef structure
            // Persisted items have empty elements Object, while fresh items have LinkRefs
            let initial_list = if let (Some(data_tmpl), Value::List(items)) = (&data_template, &initial_list) {
                let mut reconstructed_items = Vec::new();
                let mut reconstructed_elements = Vec::new();
                let mut any_reconstructed = false;

                for item in items.iter() {
                    // Check if this item needs reconstruction (empty elements field)
                    // Find the elements field dynamically (Object containing LinkRefs in template)
                    let needs_reconstruction = if let Value::Object(obj) = item {
                        // Check if any Object field is empty (elements were stripped during persistence)
                        obj.values().any(|v| {
                            matches!(v, Value::Object(inner) if inner.is_empty())
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
                        init_cell("list_elements", Value::List(std::sync::Arc::new(reconstructed_elements)));
                    }
                    Value::List(std::sync::Arc::new(reconstructed_items))
                } else {
                    initial_list.clone()
                }
            } else {
                initial_list.clone()
            };

            // Update CELL_STATES with reconstructed list
            init_cell(list_name, initial_list.clone());

            // Add list-append HOLDs manually (can't use add_list_append_on_enter which returns new Self)
            let transform = if let Some(ref data_tmpl) = data_template {
                // Discover the text CellRef field name from the template
                let text_cell_field = if let Value::Object(fields) = &data_tmpl {
                    fields.iter()
                        .find(|(_, v)| {
                            if let Value::CellRef(cell_id) = v {
                                if let Some(initial) = super::super::io::get_cell_value(&cell_id.name()) {
                                    return matches!(initial, Value::Text(_));
                                }
                            }
                            false
                        })
                        .map(|(k, _)| k.to_string())
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                zoon::println!("[DD Interpreter] Discovered text hold field: {:?}", text_cell_field);
                // Use template-based append for proper Element AST items
                StateTransform::ListAppendWithTemplate {
                    data_template: data_tmpl.clone(),
                    element_template,
                    title_cell_field: text_cell_field,
                }
            } else {
                // Fall back to simple object append
                zoon::println!("[DD Interpreter] WARNING: new_list_item function not found, using legacy ListAppend");
                StateTransform::ListAppend
            };

            config.cells.push(CellConfig {
                id: CellId::new(list_name),
                initial: initial_list.clone(),
                triggered_by: vec![LinkId::new(link_id)],
                timer_interval_ms: 0,
                filter: EventFilter::TextStartsWith("Enter:".to_string()),
                transform,
                persist: true,
            });
            // Task 7.1: Use dynamic text-clear HOLD ID (same as initialized above)
            config.cells.push(CellConfig {
                id: CellId::new(&text_clear_cell_id),
                initial: Value::text(""),
                triggered_by: vec![LinkId::new(link_id)],
                timer_interval_ms: 0,
                filter: EventFilter::TextStartsWith("Enter:".to_string()),
                transform: StateTransform::ClearText,
                persist: false,
            });
            // REMOVED: ToggleListItemCompleted - checkboxes use toggle_cell_bool() directly
            // REMOVED: SetListItemEditing - editing uses DynamicLinkAction::SetTrue directly
            // REMOVED: UpdateListItemTitle - saving uses DynamicLinkAction::EditingHandler directly

            // Remove HOLD for dynamic list item deletion (uses stable LinkRef identity)
            // Event format: "remove:LINK_ID" where LINK_ID is the remove button's LinkRef
            config.cells.push(CellConfig {
                id: CellId::new(list_name),
                initial: initial_list.clone(),
                triggered_by: vec![LinkId::new("dynamic_list_remove")],
                timer_interval_ms: 0,
                filter: EventFilter::TextStartsWith("remove:".to_string()),
                transform: StateTransform::RemoveListItem,
                persist: true,
            });
            // Note: Toggle-all is handled via HOLD body subscriptions in Boon code,
            // not through a StateTransform (see todo_mvc.bn lines 117-118)

            // Task 7.2: Dynamically detect the completed field name from template instead of hardcoding
            // NO FALLBACKS: If template exists, field MUST be found explicitly (no silent "completed" default)
            // This detection is needed BEFORE clear-completed and toggle-all registration
            let completed_field_name: Option<String> = match data_template.as_ref() {
                Some(tmpl) => {
                    let detected = find_boolean_field_in_template(tmpl);
                    if detected.is_none() {
                        zoon::println!("[DD Interpreter] WARNING: Could not detect boolean field in template. \
                            Clear-completed and toggle-all features require an explicit boolean field.");
                    }
                    detected
                },
                None => {
                    zoon::println!("[DD Interpreter] WARNING: No data template found, skipping clear-completed/toggle-all detection");
                    None
                }
            };

            // Add Clear Completed HOLD if bulk remove event path was parsed from List/remove
            // This uses PARSED CODE STRUCTURE, not UI label matching!
            // The bulk_remove_event_path is set from: List/remove(item, on: elements.X.event.press |> THEN {...})
            // Only register if we have a valid completed_field_name (NO FALLBACKS)
            let bulk_remove_path = get_bulk_remove_event_path();
            if !bulk_remove_path.is_empty() {
                if let Some(ref field_name) = completed_field_name {
                    zoon::println!("[DD Interpreter] Found bulk remove path from parsed code: {:?}, field={}", bulk_remove_path, field_name);
                    // Resolve the path to get the actual LinkRef ID from the runtime
                    if let Some(clear_completed_id) = resolve_path_to_link_ref(&runtime, &bulk_remove_path) {
                        zoon::println!("[DD Interpreter] Adding clear-completed for list: button_link={}, field={}", clear_completed_id, field_name);
                        config.cells.push(CellConfig {
                            id: CellId::new(list_name),
                            initial: initial_list,
                            triggered_by: vec![LinkId::new(&clear_completed_id)],
                            timer_interval_ms: 0,
                            filter: EventFilter::Any,
                            transform: StateTransform::ListRemoveCompleted {
                                completed_field: field_name.clone(),
                            },
                            persist: true,
                        });
                    } else {
                        zoon::println!("[DD Interpreter] WARNING: Could not resolve bulk remove path {:?}", bulk_remove_path);
                    }
                } else {
                    zoon::println!("[DD Interpreter] Skipping clear-completed (no valid completed field found)");
                }
            }

            // Register global toggle bindings (toggle-all checkbox)
            // These are extracted from HOLD bodies (like completed HOLD in each item) that contain:
            //   store.elements.toggle_all_checkbox.event.click |> THEN { store.all_completed |> Bool/not() }
            // We only need to register ONCE per unique LinkRef
            let global_toggle_bindings = get_global_toggle_bindings();
            zoon::println!("[DD Interpreter] Found {} global toggle bindings", global_toggle_bindings.len());
            let mut registered_toggle_links: std::collections::HashSet<String> = std::collections::HashSet::new();

            // Only register toggle-all actions if we successfully detected the field name
            if let Some(ref field_name) = completed_field_name {
                zoon::println!("[DD Interpreter] Detected boolean field name: {}", field_name);
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
                        use super::super::io::{add_dynamic_link_action, DynamicLinkAction};
                        // Use the detected list HOLD name (e.g., "todos"), not the individual completed HOLD ID
                        zoon::println!("[DD Interpreter] Registering toggle-all action: LinkRef={} for list {}, field={}", link_id, list_name, field_name);
                        add_dynamic_link_action(link_id, DynamicLinkAction::ListToggleAllCompleted {
                            list_cell_id: list_name.clone(),  // Use the list HOLD, not individual item HOLD
                            completed_field: field_name.clone(), // Task 7.2: Dynamic field name
                        });
                    } else {
                        zoon::println!("[DD Interpreter] WARNING: Could not resolve toggle-all path {:?}", binding.event_path);
                    }
                }
            } else {
                zoon::println!("[DD Interpreter] Skipping toggle-all registration (no valid field found)");
            }
        }
        config
    } else if let (Some(link_id), Some(list_name)) = (&key_down_link, &list_var_name) {
        // Task 7.1: Use detected list_name instead of hardcoded "items"
        // Text input with key_down pattern (shopping_list): key_down |> WHEN { Enter => append }
        // Use persisted value, or fall back to static evaluation, or empty list
        // FIX: Resolve CellRef to its actual value - DD transform expects Value::List, not CellRef
        let initial_list = load_persisted_cell_value(list_name)
            .unwrap_or_else(|| {
                match &static_items {
                    Some(Value::List(_)) => static_items.clone().unwrap(),
                    // CellRef: Look up the actual list value from CELL_STATES
                    Some(Value::CellRef(cell_id)) => {
                        super::super::io::get_cell_value(&cell_id.name())
                            .unwrap_or_else(|| Value::List(std::sync::Arc::new(Vec::new())))
                    }
                    _ => Value::List(std::sync::Arc::new(Vec::new())),
                }
            });
        // Initialize CELL_STATES so the bridge can read the initial value for reactive labels
        init_cell(list_name, initial_list.clone());
        // Task 7.1: Use dynamic text-clear HOLD ID (derived from link ID)
        let text_clear_cell_id = format!("text_clear_{}", link_id);
        // Phase 7.3: text_clear_cell now registered via DataflowConfig methods
        init_cell(&text_clear_cell_id, Value::text(""));

        // Check if there's also a clear button (List/clear pattern)
        if let Some(ref clear_link_id) = button_press_link {
            zoon::println!("[DD Interpreter] List-append-with-clear config: key_link={}, clear_link={}, text_clear={}, initial {:?}",
                link_id, clear_link_id, text_clear_cell_id, initial_list);
            DataflowConfig::new().add_list_append_with_clear(list_name, initial_list, link_id, clear_link_id, &text_clear_cell_id)
        } else {
            zoon::println!("[DD Interpreter] List-append config: link={}, text_clear={}, initial {:?}", link_id, text_clear_cell_id, initial_list);
            DataflowConfig::new().add_list_append_on_enter(list_name, initial_list, link_id, &text_clear_cell_id)
        }
    } else {
        // Link-driven pattern: button |> THEN |> HOLD/LATEST
        // Task 7.1: Use evaluator-built config with dynamic trigger IDs (no hardcoded fallback)
        // The evaluator populates triggered_by from extract_link_trigger_id()
        let has_evaluator_counter_holds = evaluator_config.cells.iter()
            .any(|h| !h.triggered_by.is_empty() && h.timer_interval_ms == 0);

        if has_evaluator_counter_holds {
            zoon::println!("[DD Interpreter] Using evaluator-built config for counter pattern ({} cells)", evaluator_config.cells.len());
            // Phase 6: init_cell removed - Worker::spawn() handles initialization synchronously
            evaluator_config
        } else {
            // Fallback for patterns not yet handled by evaluator
            zoon::println!("[DD Interpreter] WARNING: No evaluator config, using legacy fallback");
            let cell_id = "hold_0";
            let link_id = "link_1";
            let initial_value = load_persisted_cell_value(cell_id).unwrap_or_else(|| Value::int(0));
            // Phase 6: init_cell removed - Worker::spawn() handles initialization synchronously
            zoon::println!("[DD Interpreter] Legacy counter config: {} triggered by {}, initial {:?}", cell_id, link_id, initial_value);
            DataflowConfig::counter_with_initial_hold(link_id, cell_id, initial_value)
        }
    };

    let worker_handle = Worker::with_config(config).spawn();

    // Phase 6: Split returns just (event_input, task_handle) - no output channel needed
    let (event_input, task_handle) = worker_handle.split();

    // Set up global dispatcher so button clicks inject events
    let injector = EventInjector::new(event_input);
    set_global_dispatcher(injector.clone());

    // If timer-driven, start JavaScript timer to fire events
    if let Some((ref _cell_id, interval_ms)) = timer_info {
        let timer_injector = injector.clone();
        let timer_handle = Task::start_droppable(async move {
            let mut tick: u64 = 0;
            loop {
                zoon::Timer::sleep(interval_ms as u32).await;
                tick += 1;
                timer_injector.fire_timer(super::super::core::TimerId::new(interval_ms.to_string()), tick);
                zoon::println!("[DD Timer] Tick {} for {}ms timer", tick, interval_ms);
            }
        });
        // Store timer handle separately to keep it alive
        set_timer_handle(timer_handle);
        zoon::println!("[DD Interpreter] Timer started: {}ms interval", interval_ms);
    }

    // Store task handle to keep the async worker alive
    set_task_handle(task_handle);

    zoon::println!("[DD Interpreter] Worker started, dispatcher configured (Phase 6: single state authority)");

    Some(DdResult {
        document,
        context: DdContext::new(),
    })
}

/// Detect the list variable from the runtime by searching for List values.
///
/// Returns (list_value, variable_name) if found.
/// Checks common patterns in order: store.*, then top-level variables.
fn detect_list_variable(runtime: &BoonDdRuntime) -> (Option<Value>, Option<String>) {
    // First check if there's a "store" object with list fields
    if let Some(store) = runtime.get_variable("store") {
        if let Value::Object(fields) = store {
            // Look for any field that contains a List value or CellRef to a list
            for (field_name, value) in fields.iter() {
                if matches!(value, Value::List(_)) {
                    return (Some(value.clone()), Some(field_name.to_string()));
                }
                // Also check for CellRef - this is a reactive list like store.todos in todo_mvc
                if let Value::CellRef(cell_id) = value {
                    // Return the CellRef and the cell_id as the list name
                    zoon::println!("[DD Interpreter] Found CellRef in store.{}: {}", field_name, cell_id);
                    return (Some(value.clone()), Some(cell_id.to_string()));
                }
            }
        }
    }

    // Task 7.1: Removed hardcoded priority search for ["items", "list", "data"]
    // Now searches ALL top-level variables for List values (generic)
    for (name, value) in runtime.get_all_variables() {
        if matches!(value, Value::List(_)) {
            return (Some(value.clone()), Some(name.clone()));
        }
    }

    (None, None)
}

/// Task 7.2: Find the field name for a boolean CellRef in a template.
/// Given a template object and a HOLD ID, returns the field name that contains that CellRef.
/// Used to dynamically determine "completed" field name instead of hardcoding.
fn find_boolean_field_in_template(template: &Value) -> Option<String> {
    match template {
        Value::Object(fields) => {
            // Look for a field with a boolean initial value (CellRef pointing to bool HOLD)
            for (field_name, value) in fields.iter() {
                if let Value::CellRef(cell_id) = value {
                    // Check if this HOLD has a boolean initial value
                    if let Some(hold_value) = super::super::io::get_cell_value(&cell_id.name()) {
                        if matches!(hold_value, Value::Bool(_)) {
                            // This is likely the "completed" field - return its name
                            zoon::println!("[DD Interpreter] Found boolean field: {} -> {}", field_name, cell_id);
                            return Some(field_name.to_string());
                        }
                    }
                }
            }
            None
        }
        Value::Tagged { fields, .. } => {
            // Same logic for Tagged objects
            for (field_name, value) in fields.iter() {
                if let Value::CellRef(cell_id) = value {
                    if let Some(hold_value) = super::super::io::get_cell_value(&cell_id.name()) {
                        if matches!(hold_value, Value::Bool(_)) {
                            zoon::println!("[DD Interpreter] Found boolean field: {} -> {}", field_name, cell_id);
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
    cell_id: String,
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
    fn traverse_path(start: &Value, path: &[String]) -> Option<String> {
        let mut current = start.clone();
        for segment in path {
            match &current {
                Value::Object(fields) => {
                    current = fields.get(segment.as_str())?.clone();
                }
                Value::Tagged { fields, .. } => {
                    current = fields.get(segment.as_str())?.clone();
                }
                _ => return None,
            }
        }
        // The final value should be a LinkRef
        if let Value::LinkRef(link_id) = current {
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

/// Extract the element_template from list mapping Tagged values in the document.
/// Pure DD: This returns the template that was created by the evaluator with __placeholder_while__ Tagged structures.
fn extract_element_template_from_document(document: &Option<Value>) -> Option<Value> {
    let doc = document.as_ref()?;
    extract_element_template_from_value(doc)
}

/// Recursively search for and extract element_template from list mapping Tagged values.
/// Pure DD: List refs are now Tagged values with special tags.
fn extract_element_template_from_value(value: &Value) -> Option<Value> {
    match value {
        Value::Object(fields) => {
            for field_value in fields.values() {
                if let Some(template) = extract_element_template_from_value(field_value) {
                    return Some(template);
                }
            }
            None
        }
        Value::List(items) => {
            for item in items.iter() {
                if let Some(template) = extract_element_template_from_value(item) {
                    return Some(template);
                }
            }
            None
        }
        Value::Tagged { tag, fields, .. } => {
            // Check for list mapping Tagged values (pure DD replacements)
            match tag.as_ref() {
                "__while_config__" => {
                    // Search in arms and default
                    if let Some(Value::List(arms)) = fields.get("arms") {
                        for item in arms.iter() {
                            if let Some(body) = item.get("body") {
                                if let Some(template) = extract_element_template_from_value(body) {
                                    return Some(template);
                                }
                            }
                        }
                    }
                    if let Some(default) = fields.get("default") {
                        if let Some(template) = extract_element_template_from_value(default) {
                            return Some(template);
                        }
                    }
                    None
                }
                "__mapped_list__" | "__filtered_mapped_list__" => {
                    // Extract element_template from these Tagged values
                    fields.get("element_template").cloned()
                }
                _ => {
                    // Regular Tagged value - search in fields
                    for field_value in fields.values() {
                        if let Some(template) = extract_element_template_from_value(field_value) {
                            return Some(template);
                        }
                    }
                    None
                }
            }
        }
        _ => None,
    }
}

// Task 6.3: extract_checkbox_toggles_from_value DELETED - evaluator provides toggle bindings directly

/// Information about an editing toggle pattern.
/// Used for list_example style patterns where double-click enters editing mode.
struct EditingToggle {
    /// The HOLD ID that controls editing state (e.g., "hold_10")
    cell_id: String,
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
