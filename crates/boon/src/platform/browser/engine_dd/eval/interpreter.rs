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
use std::collections::HashMap;
use super::evaluator::BoonDdRuntime;
use super::super::core::value::{CollectionHandle, Value, WhileConfig, PlaceholderWhileConfig, WhileArm};
use super::super::core::{Worker, DataflowConfig, CellConfig, CellId, LinkId, EventFilter, StateTransform, reconstruct_persisted_item, instantiate_fresh_item, remap_link_mappings_for_item, LinkAction, LinkCellMapping, Key, ITEM_KEY_FIELD, get_link_ref_at_path, ROUTE_CHANGE_LINK_ID};
// Phase 7.3: Removed imports of deleted setter/clear functions (now in DataflowConfig)
use super::super::io::{
    EventInjector, set_global_dispatcher, clear_global_dispatcher,
    set_task_handle, clear_task_handle, clear_output_listener_handle,
    set_timer_handle, clear_timer_handle,
    load_persisted_list_items_with_collections, clear_cells_memory,
    // Getters only - setters removed (now via DataflowConfig)
    init_current_route, get_current_route,
};
// Phase 11a: clear_router_mappings was removed - routing goes through DD dataflow now
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
        Value::PlaceholderField(_) => true,
        Value::WhileConfig(config) => {
            let arms_have_holds = config.arms.iter().any(|arm| has_dynamic_holds(&arm.body));
            let default_has_holds = has_dynamic_holds(&config.default);
            arms_have_holds || default_has_holds
        }
        Value::PlaceholderWhile(config) => {
            let arms_have_holds = config.arms.iter().any(|arm| has_dynamic_holds(&arm.body));
            let default_has_holds = has_dynamic_holds(&config.default);
            arms_have_holds || default_has_holds
        }
        Value::Object(fields) => fields.values().any(has_dynamic_holds),
        Value::Tagged { fields, .. } => fields.values().any(has_dynamic_holds),
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
    // Dynamic link registry removed - no-op
    // Phase 7.3: Config clearing now handled by worker lifecycle
    // Removed: clear_remove_event_path, clear_bulk_remove_bindings,
    //          clear_editing_event_bindings, clear_toggle_event_bindings, clear_global_toggle_bindings
    // DELETED: clear_checkbox_toggle_holds() - registry was dead code (set but never read)
    // Text input clearing is handled by Boon code, no IO registry to clear.
    clear_cells_memory();  // Prevent state contamination between examples

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
        panic!("[DD Interpreter] Lexing failed for {}", filename);
    }
    let Some(mut tokens) = tokens else {
        panic!("[DD Interpreter] Lexer produced no tokens for {}", filename);
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
        panic!("[DD Interpreter] Parsing failed for {}", filename);
    }
    let Some(ast) = ast else {
        panic!("[DD Interpreter] Parser produced no AST for {}", filename);
    };

    // Step 3: Resolve references
    let ast = match resolve_references(ast) {
        Ok(ast) => ast,
        Err(errors) => {
            zoon::eprintln!("[DD Interpreter] Reference errors:");
            for err in &errors {
                zoon::eprintln!("  {:?}", err);
            }
            panic!("[DD Interpreter] Reference resolution failed for {}", filename);
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
            panic!("[DD Interpreter] Persistence resolution failed for {}", filename);
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
    let mut evaluator_config = runtime.take_config();
    // Attach element templates to list item templates (fail-fast if missing).
    evaluator_config.attach_list_element_templates();
    evaluator_config.register_collection_cells();
    zoon::println!("[DD Interpreter] Evaluator built {} CellConfig entries", evaluator_config.cells.len());
    for (i, cell) in evaluator_config.cells.iter().enumerate() {
        zoon::println!("[DD Interpreter]   [{}] id={}, transform={:?}, timer={}ms",
            i, cell.id.name(), cell.transform, cell.timer_interval_ms);
    }

    // Initialize route cells (if Router/route() was used) in the interpreter, not evaluator.
    if !evaluator_config.route_cells.is_empty() {
        let route_cells = evaluator_config.route_cells.clone();
        #[cfg(target_arch = "wasm32")]
        {
            init_current_route();
            let path = get_current_route();
            for cell_id in route_cells.iter() {
                evaluator_config.add_cell_initialization(cell_id.clone(), Value::text(path.clone()), false);
                evaluator_config.add_link_mapping(LinkCellMapping::new(
                    ROUTE_CHANGE_LINK_ID,
                    cell_id.clone(),
                    LinkAction::SetText,
                ));
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            for cell_id in route_cells.iter() {
                evaluator_config.add_cell_initialization(cell_id.clone(), Value::text("/"), false);
                evaluator_config.add_link_mapping(LinkCellMapping::new(
                    ROUTE_CHANGE_LINK_ID,
                    cell_id.clone(),
                    LinkAction::SetText,
                ));
            }
        }
    }

    // Detect list cells from evaluator-built config (no runtime scanning).
    let (static_list, list_var_name) = detect_list_cell_from_config(&evaluator_config);
    zoon::println!("[DD Interpreter] Detected list cell from config: {:?}", list_var_name);
    let static_items = static_list.clone();

    zoon::println!("[DD Interpreter] Evaluation complete, has document: {}",
        document.is_some());
    zoon::println!("[DD Interpreter] static_list = {:?}", static_list);
    zoon::println!("[DD Interpreter] static_items = {:?}", static_items);

    // Step 7: Set up Worker for reactive updates
    // Task 4.3: Prefer evaluator-built config over extract_* pattern detection

    // Check if evaluator built timer HOLDs (timer_interval_ms > 0)
    let has_timer_hold = evaluator_config.cells.iter().any(|h| h.timer_interval_ms > 0);

    // Link actions are encoded directly as LinkCellMapping in evaluator config.

    // Task 6.3: Get timer info from evaluator-built config ONLY (no fallback)
    let timer_info: Option<(String, u64)> = evaluator_config.cells.iter()
        .find(|h| h.timer_interval_ms > 0)
        .map(|h| (h.id.name().to_string(), h.timer_interval_ms));
    // Task 6.3: List/append bindings parsed by evaluator (no IO registries)
    if evaluator_config.list_append_bindings.len() > 1 {
        panic!("[DD Interpreter] Multiple List/append bindings detected; explicit handling required.");
    }
    let key_down_link = evaluator_config.list_append_bindings.first().and_then(|binding| {
        if binding.append_link_ids.len() > 1 {
            panic!("[DD Interpreter] Multiple append links for list '{}': {:?}", binding.list_cell_id, binding.append_link_ids);
        }
        binding.append_link_ids.first().cloned()
    });
    if key_down_link.is_some() {
        zoon::println!("[DD Interpreter] Using evaluator-provided List/append link: {:?}", key_down_link);
    }
    if key_down_link.is_some() && list_var_name.is_none() {
        panic!("[DD Interpreter] Bug: List/append binding present but no list cell detected");
    }
    // Prefer explicit List/clear bindings from parsed code.
    let button_press_link = evaluator_config.list_append_bindings.first().and_then(|binding| {
        if binding.clear_link_ids.len() > 1 {
            panic!("[DD Interpreter] Multiple clear links for list '{}': {:?}", binding.list_cell_id, binding.clear_link_ids);
        }
        binding.clear_link_ids.first().cloned()
    });
    if button_press_link.is_some() {
        zoon::println!("[DD Interpreter] Using evaluator-provided List/clear link: {:?}", button_press_link);
    }
    if button_press_link.is_some() && list_var_name.is_none() {
        panic!("[DD Interpreter] Bug: List/clear binding present but no list cell detected");
    }
    let list_name = list_var_name.clone();
    let has_append_binding = list_name.as_ref().map(|name| {
        evaluator_config.list_append_bindings.iter().any(|binding| binding.list_cell_id == *name)
    }).unwrap_or(false);
    let has_remove_path = list_name.as_ref().map(|name| {
        evaluator_config.remove_event_paths.contains_key(name)
    }).unwrap_or(false);
    let has_list_template = list_name.as_ref().map(|name| {
        evaluator_config.list_item_templates.contains_key(name)
    }).unwrap_or(false);

    if list_var_name.is_some() && !has_append_binding && !has_remove_path {
        panic!("[DD Interpreter] Bug: reactive list detected but no List/append or List/remove bindings were parsed.");
    }
    if has_append_binding && !has_list_template {
        let list_id = list_name.as_ref().map(|s| s.as_str()).unwrap_or("<unknown>");
        panic!(
            "[DD Interpreter] List/append requires ListItemTemplate for list '{}'",
            list_id
        );
    }
    // Task 6.3: Checkbox and editing toggles are derived from evaluator config only.
    // NOTE: clear_completed_link is no longer extracted by label matching!
    // Bulk remove bindings are parsed into DataflowConfig.bulk_remove_bindings

    let config = if has_timer_hold {
        // Task 4.3: Use evaluator-built config for timer patterns
        // This eliminates extract_timer_info() for timer-driven patterns
        zoon::println!("[DD Interpreter] Using evaluator-built config for timer pattern ({} cells)", evaluator_config.cells.len());
        evaluator_config
    } else if let Some(list_name) = list_var_name.clone() {
        let mut config = evaluator_config.clone();
        config.attach_list_element_templates();

        // Editing link actions are already encoded in evaluator config link mappings.
        let list_template = config.get_list_item_template(&list_name).cloned();
        if list_template.is_some() && !has_append_binding {
            panic!(
                "[DD Interpreter] Bug: list template present but no List/append binding for '{}'",
                list_name
            );
        }
        if has_append_binding && key_down_link.is_none() {
            panic!("[DD Interpreter] Bug: list append binding present but no List/append link");
        }

        let initial_cell_values = build_initial_cell_values(&config);
        let remove_path = config.remove_event_paths.get(&list_name).cloned().unwrap_or_else(|| {
            panic!("[DD Interpreter] Bug: missing remove identity path for list '{}'", list_name);
        });
        if let Some(template) = list_template.as_ref() {
            if template.identity.link_ref_path != remove_path {
                panic!(
                    "[DD Interpreter] Bug: remove identity path mismatch for list '{}': {:?} vs {:?}",
                    list_name, template.identity.link_ref_path, remove_path
                );
            }
        }

        let collection_id = config
            .collection_sources
            .iter()
            .find_map(|(id, cell)| if cell == &list_name { Some(*id) } else { None })
            .unwrap_or_else(|| {
                panic!("[DD Interpreter] Bug: missing collection id for list '{}'", list_name);
            });

        // Use persisted list items (with nested collections), or fall back to evaluator-provided initial collection.
        let initial_items_raw: Vec<Value> = if let Some(persisted) = load_persisted_list_items_with_collections(&list_name) {
            for (collection_id, items) in persisted.collections {
                if config.initial_collections.insert(collection_id, items).is_some() {
                    panic!(
                        "[DD Interpreter] Bug: duplicate nested collection id {:?} while loading '{}'",
                        collection_id, list_name
                    );
                }
            }
            persisted.items
        } else {
            match &static_items {
                Some(Value::List(handle)) => {
                    if handle.id != collection_id {
                        panic!(
                            "[DD Interpreter] Bug: collection id mismatch for '{}': expected {:?}, found {:?}",
                            list_name, collection_id, handle.id
                        );
                    }
                    if let Some(existing) = handle.cell_id.as_deref() {
                        if existing != list_name {
                            panic!(
                                "[DD Interpreter] Bug: collection cell_id mismatch for '{}': found '{}'",
                                list_name, existing
                            );
                        }
                    }
                    config.initial_collections.get(&collection_id).cloned().unwrap_or_else(|| {
                        panic!(
                            "[DD Interpreter] Bug: missing initial items for list '{}'",
                            list_name
                        );
                    })
                }
                Some(Value::CellRef(cell_id)) => {
                    let value = get_initial_cell_value(&evaluator_config, &cell_id.name())
                        .unwrap_or_else(|| {
                            panic!("[DD Interpreter] Bug: missing initial list value for '{}'", list_name);
                        });
                    let Value::List(handle) = value else {
                        panic!(
                            "[DD Interpreter] Bug: list '{}' initial value must be Collection, found {:?}",
                            list_name, value
                        );
                    };
                    if handle.id != collection_id {
                        panic!(
                            "[DD Interpreter] Bug: collection id mismatch for '{}': expected {:?}, found {:?}",
                            list_name, collection_id, handle.id
                        );
                    }
                    config.initial_collections.get(&collection_id).cloned().unwrap_or_else(|| {
                        panic!(
                            "[DD Interpreter] Bug: missing initial items for list '{}'",
                            list_name
                        );
                    })
                }
                _ => panic!("[DD Interpreter] Bug: missing initial list value for '{}'", list_name),
            }
        };

        let mut initial_items = initial_items_raw.clone();

        // Reconstruct persisted items that lost their CellRef structure (template lists only).
        if let Some(list_template) = list_template.as_ref() {
            let mut reconstructed_items = Vec::new();
            let mut item_initializations: Vec<(String, Value)> = Vec::new();

            for item in initial_items_raw.iter() {
                let needs_reconstruction = if let Value::Object(obj) = item {
                    obj.values().any(|v| matches!(v, Value::Object(inner) if inner.is_empty()))
                } else {
                    false
                };

                if needs_reconstruction {
                    let Some(instantiated) = reconstruct_persisted_item(
                        item,
                        &list_template.data_template,
                        list_template.element_template.as_ref(),
                        &list_template.identity.link_ref_path,
                        &mut config.initial_collections,
                    ) else {
                        panic!("[DD Interpreter] Bug: Failed to reconstruct persisted item.");
                    };
                    // Remap template link mappings to this item's fresh IDs.
                    let mut remapped = remap_link_mappings_for_item(
                        &config.link_mappings,
                        &instantiated.link_id_map,
                        &instantiated.cell_id_map,
                    );
                    remapped.extend(instantiated.link_mappings.clone());
                    for mapping in remapped {
                        config.add_link_mapping(mapping);
                    }
                    item_initializations.extend(instantiated.initializations);
                    reconstructed_items.push(instantiated.data);
                } else {
                    let Some(instantiated) = instantiate_fresh_item(
                        item,
                        list_template.element_template.as_ref(),
                        &list_template.identity.link_ref_path,
                        &initial_cell_values,
                        &mut config.initial_collections,
                    ) else {
                        panic!("[DD Interpreter] Bug: Failed to instantiate fresh item.");
                    };
                    // Remap template link mappings to this item's fresh IDs.
                    let mut remapped = remap_link_mappings_for_item(
                        &config.link_mappings,
                        &instantiated.link_id_map,
                        &instantiated.cell_id_map,
                    );
                    remapped.extend(instantiated.link_mappings.clone());
                    for mapping in remapped {
                        config.add_link_mapping(mapping);
                    }
                    item_initializations.extend(instantiated.initializations);
                    reconstructed_items.push(instantiated.data);
                }
            }

            for (cell_id, value) in item_initializations {
                if config.cells.iter().any(|cell| cell.id.name() == cell_id) {
                    panic!("[DD Interpreter] Bug: duplicate cell config for '{}'", cell_id);
                }
                config.cells.push(CellConfig {
                    id: CellId::new(&cell_id),
                    initial: value,
                    triggered_by: Vec::new(),
                    timer_interval_ms: 0,
                    filter: EventFilter::Any,
                    transform: StateTransform::Identity,
                    persist: false,
                });
            }

            initial_items = reconstructed_items;
        } else {
            let needs_reconstruction = initial_items.iter().any(|item| {
                if let Value::Object(obj) = item {
                    obj.values().any(|v| matches!(v, Value::Object(inner) if inner.is_empty()))
                } else {
                    false
                }
            });
            if needs_reconstruction {
                panic!(
                    "[DD Interpreter] Bug: persisted items require reconstruction but no ListItemTemplate for '{}'",
                    list_name
                );
            }
        }
        config.initial_collections.insert(collection_id, initial_items.clone());
        let initial_list = Value::List(CollectionHandle::with_id_and_cell(
            collection_id,
            list_name.as_str(),
        ));

        // Register RemoveListItem mappings for initial items using the configured identity path.
        for item in &initial_items {
            let link_id = get_link_ref_at_path(item, &remove_path)
                .unwrap_or_else(|| {
                    panic!(
                        "[DD Interpreter] Bug: identity path {:?} did not resolve in initial item",
                        remove_path
                    );
                });
            let expected_key = format!("link:{}", link_id);
            let key_value = match item {
                Value::Object(fields) => fields.get(ITEM_KEY_FIELD),
                Value::Tagged { fields, .. } => fields.get(ITEM_KEY_FIELD),
                other => {
                    panic!(
                        "[DD Interpreter] Bug: list item must be Object/Tagged, found {:?}",
                        other
                    );
                }
            }.unwrap_or_else(|| {
                panic!(
                    "[DD Interpreter] Bug: list item missing '{}' for remove mapping",
                    ITEM_KEY_FIELD
                );
            });
            match key_value {
                Value::Text(key) if key.as_ref() == expected_key => {}
                other => {
                    panic!(
                        "[DD Interpreter] Bug: list item '{}' mismatch: expected '{}', found {:?}",
                        ITEM_KEY_FIELD, expected_key, other
                    );
                }
            }
            config.add_link_mapping(LinkCellMapping::remove_list_item(
                link_id,
                list_name.clone(),
            ));
        }

        // Toggle/set actions are encoded in evaluator config link mappings and remapped per item.

        if has_append_binding {
            let link_id = key_down_link.clone().unwrap_or_else(|| {
                panic!("[DD Interpreter] Bug: list template present but no List/append link");
            });
            let list_template = list_template.unwrap_or_else(|| {
                panic!("[DD Interpreter] Bug: missing list item template for '{}'", list_name);
            });
            // Add list append HOLD config (template-based)
            if let Some(ref clear_link_id) = button_press_link {
                config.cells.push(CellConfig {
                    id: CellId::new(&list_name),
                    initial: initial_list.clone(),
                    triggered_by: vec![LinkId::new(&link_id), LinkId::new(clear_link_id)],
                    timer_interval_ms: 0,
                    filter: EventFilter::Any,
                    transform: StateTransform::ListAppendWithTemplateAndClear {
                        template: list_template.clone(),
                        clear_link_id: clear_link_id.to_string(),
                    },
                    persist: true,
                });
            } else {
                config.cells.push(CellConfig {
                    id: CellId::new(&list_name),
                    initial: initial_list.clone(),
                    triggered_by: vec![LinkId::new(&link_id)],
                    timer_interval_ms: 0,
                    filter: EventFilter::KeyEquals(Key::Enter),
                    transform: StateTransform::ListAppendWithTemplate {
                        template: list_template.clone(),
                    },
                    persist: true,
                });
            }
        } else {
            // Remove-only list: create identity HOLD so link mappings can apply.
            config.cells.push(CellConfig {
                id: CellId::new(&list_name),
                initial: initial_list.clone(),
                triggered_by: Vec::new(),
                timer_interval_ms: 0,
                filter: EventFilter::Any,
                transform: StateTransform::Identity,
                persist: true,
            });
        }

        config
    } else {
        // Link-driven pattern: button |> THEN |> HOLD/LATEST
        // Task 7.1: Use evaluator-built config with dynamic trigger IDs (no hardcoded fallback)
        // The evaluator populates triggered_by from extract_link_trigger_id()
        let has_evaluator_counter_holds = evaluator_config.cells.iter()
            .any(|h| !h.triggered_by.is_empty() && h.timer_interval_ms == 0);
        let has_evaluator_link_mappings = !evaluator_config.link_mappings.is_empty();

        if has_evaluator_counter_holds || has_evaluator_link_mappings {
            zoon::println!(
                "[DD Interpreter] Using evaluator-built config (cells: {}, link_mappings: {})",
                evaluator_config.cells.len(),
                evaluator_config.link_mappings.len()
            );
            // Phase 6: init_cell removed - Worker::spawn() handles initialization synchronously
            evaluator_config
        } else {
            panic!("[DD Interpreter] Bug: No evaluator config for link-driven pattern.");
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

/// Detect list cells from evaluator-built config (no runtime scanning).
///
/// Returns (initial_list_value, cell_id) if found.
fn detect_list_cell_from_config(config: &DataflowConfig) -> (Option<Value>, Option<String>) {
    if config.list_append_bindings.len() > 1 {
        let ids: Vec<String> = config.list_append_bindings.iter().map(|b| b.list_cell_id.clone()).collect();
        panic!("[DD Interpreter] Multiple List/append bindings detected: {:?}", ids);
    }
    if let Some(binding) = config.list_append_bindings.first() {
        let initial = get_initial_cell_value(config, &binding.list_cell_id)
            .unwrap_or_else(|| {
                panic!("[DD Interpreter] Bug: missing initial list value for '{}'", binding.list_cell_id);
            });
        return (Some(initial), Some(binding.list_cell_id.clone()));
    }
    if config.remove_event_paths.len() > 1 {
        let ids: Vec<String> = config.remove_event_paths.keys().cloned().collect();
        panic!("[DD Interpreter] Multiple List/remove bindings detected: {:?}", ids);
    }
    if let Some((list_id, _path)) = config.remove_event_paths.iter().next() {
        let initial = get_initial_cell_value(config, list_id)
            .unwrap_or_else(|| {
                panic!("[DD Interpreter] Bug: missing initial list value for '{}'", list_id);
            });
        return (Some(initial), Some(list_id.clone()));
    }
    (None, None)
}

/// Look up an initial value for a cell from the evaluator-built config.
fn get_initial_cell_value(config: &DataflowConfig, cell_id: &str) -> Option<Value> {
    config
        .cells
        .iter()
        .find(|cell| cell.id.name() == cell_id)
        .map(|cell| cell.initial.clone())
        .or_else(|| config.get_cell_initialization(cell_id).map(|init| init.value.clone()))
}

/// Build a map of initial cell values from the evaluator config.
fn build_initial_cell_values(config: &DataflowConfig) -> HashMap<String, Value> {
    let mut values = HashMap::new();
    for cell in &config.cells {
        values.insert(cell.id.name(), cell.initial.clone());
    }
    for (cell_id, init) in &config.cell_initializations {
        values.insert(cell_id.clone(), init.value.clone());
    }
    values
}

// Task 6.3: extract_timer_info DELETED - evaluator builds timer config directly
// Task 6.3: extract_text_input_key_down DELETED - evaluator parses List/append bindings

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
        if let Some(result) = traverse_path(start, &path[1..]) {
            return Some(result);
        }
    }

    None
}

// Task 6.3: extract_checkbox_toggles_from_value DELETED - evaluator provides toggle bindings directly
// Task 6.3: extract_editing_toggles DELETED - evaluator provides editing bindings directly
// Note: toggle-all patterns are rejected; must be expressed via pure DD list/map.

// Task 6.3: extract_key_down_from_value DELETED - evaluator parses List/append bindings
// Task 6.3: extract_button_press_link DELETED - evaluator parses List/clear bindings
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
