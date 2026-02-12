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
//! 5. Return `DdResult` with document
//!
//! # Current Limitations
//!
//! - Static evaluation only (no reactive LINK events yet)
//! - No timer support yet
//! - No persistence support yet
//!
//! These will be added in subsequent phases using Worker.

use chumsky::Parser as _;
use chumsky::input::Stream;
use std::collections::HashMap;
#[allow(unused_imports)]
use super::super::dd_log;
use super::evaluator::BoonDdRuntime;
use super::super::core::value::{CollectionHandle, Value};
use super::super::core::{Worker, DataflowConfig, CellConfig, CellId, LinkId, EventFilter, StateTransform, reconstruct_persisted_item, instantiate_fresh_item, remap_link_mappings_for_item, LinkAction, LinkCellMapping, Key, ITEM_KEY_FIELD, get_link_ref_at_path, ROUTE_CHANGE_LINK_ID};
use super::super::core::dataflow::shutdown_persistent_worker;
use super::super::io::{
    EventInjector, set_global_dispatcher, clear_global_dispatcher,
    set_task_handle, clear_task_handle, clear_output_listener_handle,
    set_timer_handle, clear_timer_handle,
    load_persisted_list_items_with_collections, clear_cells_memory,
    // Getters only - setters removed (now via DataflowConfig)
    init_current_route, get_current_route,
};
use serde_json_any_key::MapIterToJson;
use zoon::{Task, StreamExt, WebStorage, local_storage};
use crate::parser::{
    Expression, Input, SourceCode, Spanned, Token, lexer, parser, reset_expression_depth,
    resolve_persistence, resolve_references, span_at, static_expression,
};

const DD_OLD_CODE_KEY: &str = "dd_old_code";
const DD_SPAN_IDS_KEY: &str = "dd_span_ids";

/// Result of running DD reactive evaluation.
#[derive(Clone)]
pub struct DdResult {
    /// The document value if evaluation succeeded
    pub document: Option<Value>,
}

/// Check if a Value contains Unit anywhere in its immediate children.
/// Used to detect already-instantiated items where LinkRefs were stripped to Unit.
fn contains_unit(value: &Value) -> bool {
    match value {
        Value::Object(fields) => fields.values().any(|v| matches!(v, Value::Unit)),
        Value::Tagged { fields, .. } => fields.values().any(|v| matches!(v, Value::Unit)),
        _ => false,
    }
}

/// Check if a persisted value should be skipped during overlay (stripped LinkRef structure).
/// Returns true if the value is Unit or an Object/Tagged where all values are Unit.
fn is_stripped_linkref_structure(value: &Value) -> bool {
    match value {
        Value::Unit => true,
        Value::Object(fields) => !fields.is_empty() && fields.values().all(|v| matches!(v, Value::Unit)),
        _ => false,
    }
}

/// Parse old source code into AST for persistence ID matching.
fn parse_old_dd<'old_code>(
    filename: &str,
    source_code: &'old_code str,
) -> Option<Vec<Spanned<Expression<'old_code>>>> {
    let (tokens, _lex_errors) = lexer().parse(source_code).into_output_errors();
    let Some(mut tokens) = tokens else {
        return None;
    };
    tokens.retain(|spanned_token| !matches!(spanned_token.node, Token::Comment(_)));

    reset_expression_depth();
    let (ast, _parse_errors) = parser()
        .parse(Stream::from_iter(tokens).map(
            span_at(source_code.len()),
            |Spanned { node, span, persistence: _ }| (node, span),
        ))
        .into_output_errors();
    let Some(ast) = ast else {
        return None;
    };

    match resolve_references(ast) {
        Ok(ast) => Some(ast),
        Err(_) => None,
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
    dd_log!("[DD Interpreter] Parsing: {}", filename);

    // Clean up any existing components from previous runs
    // This ensures old timers/workers stop before new ones start
    shutdown_persistent_worker();
    clear_timer_handle();
    clear_output_listener_handle();
    clear_task_handle();
    clear_global_dispatcher();
    // Config clearing is handled by worker lifecycle
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

    // Step 4: Resolve persistence (load old AST for stable IDs across page reloads)
    let old_source_code = local_storage().get::<String>(DD_OLD_CODE_KEY);
    let old_ast = if let Some(Ok(old_source_code)) = &old_source_code {
        parse_old_dd(filename, old_source_code)
    } else {
        None
    };

    let source_code_for_storage = source_code.to_string();

    let (ast, new_span_id_pairs) = match resolve_persistence(ast, old_ast, DD_SPAN_IDS_KEY) {
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
    dd_log!("[DD Interpreter] Evaluator built {} CellConfig entries, {} initial_collections, {} collection_ops, {} link_mappings",
        evaluator_config.cells.len(), evaluator_config.initial_collections.len(),
        evaluator_config.collection_ops.len(), evaluator_config.link_mappings.len());
    for (i, cell) in evaluator_config.cells.iter().enumerate() {
        dd_log!("[DD Interpreter]   [{}] id={}, transform={:?}, timer={}ms",
            i, cell.id.name(), cell.transform, cell.timer_interval_ms);
    }
    for (i, mapping) in evaluator_config.link_mappings.iter().enumerate() {
        dd_log!(
            "[DD Interpreter]   [mapping {}] link={} cell={} action={:?} key_filter={:?}",
            i,
            mapping.link_id,
            mapping.cell_id,
            mapping.action,
            mapping.key_filter,
        );
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
    dd_log!("[DD Interpreter] Detected list cell from config: {:?}", list_var_name);
    let static_items = static_list.clone();

    dd_log!("[DD Interpreter] Evaluation complete, has document: {}",
        document.is_some());
    dd_log!("[DD Interpreter] static_list = {:?}", static_list);
    dd_log!("[DD Interpreter] static_items = {:?}", static_items);

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
        dd_log!("[DD Interpreter] Using evaluator-provided List/append link: {:?}", key_down_link);
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
        dd_log!("[DD Interpreter] Using evaluator-provided List/clear link: {:?}", button_press_link);
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
    // Simple append+clear lists (no per-item remove) may not have a template.
    // Template is only required when List/remove exists (per-item identity tracking).
    // Task 6.3: Checkbox and editing toggles are derived from evaluator config only.
    // NOTE: clear_completed_link is no longer extracted by label matching!
    // Bulk remove bindings are parsed into DataflowConfig.bulk_remove_bindings

    let config = if has_timer_hold {
        // Task 4.3: Use evaluator-built config for timer patterns
        // This eliminates extract_timer_info() for timer-driven patterns
        dd_log!("[DD Interpreter] Using evaluator-built config for timer pattern ({} cells)", evaluator_config.cells.len());
        Some(evaluator_config)
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
        let remove_path = config.remove_event_paths.get(&list_name).cloned()
            .unwrap_or_default(); // Empty for append+clear lists (no per-item remove)
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
        dd_log!("[DD Interpreter] Loading items for list '{}', collection_id={:?}", list_name, collection_id);
        dd_log!("[DD Interpreter] static_items = {:?}", static_items);
        if let Some(items) = config.initial_collections.get(&collection_id) {
            dd_log!("[DD Interpreter] initial_collections has {} items for {:?}", items.len(), collection_id);
            for (i, item) in items.iter().enumerate() {
                dd_log!("[DD Interpreter]   raw_item[{}] = {:?}", i, item);
            }
        } else {
            dd_log!("[DD Interpreter] initial_collections MISSING for {:?}", collection_id);
        }
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

            // Debug: dump all items before processing loop
            for (idx, item) in initial_items_raw.iter().enumerate() {
                dd_log!("[DD Interpreter] BEFORE LOOP list='{}' item[{}] = {:?}", list_name, idx, item);
                if let Value::Object(obj) = item {
                    dd_log!("[DD Interpreter] BEFORE LOOP list='{}' item[{}] has_key={}, fields={:?}",
                        list_name, idx, obj.contains_key("__key"), obj.keys().collect::<Vec<_>>());
                }
            }

            // Clone per-item cell value snapshots for this collection (avoids borrow conflict
            // with &mut config.initial_collections used in instantiate_fresh_item)
            let per_item_snapshots = config.per_item_cell_values.get(&collection_id).cloned();

            for (item_idx, item) in initial_items_raw.iter().enumerate() {
                let needs_reconstruction = if let Value::Object(obj) = item {
                    // Case 1: Items deserialized from persistence that lost CellRef structure
                    // (CellRefs become empty Objects `{}` during JSON roundtrip)
                    obj.values().any(|v| matches!(v, Value::Object(inner) if inner.is_empty()))
                    // Case 2: Items already instantiated (CellRefs resolved, LinkRefs stripped to Unit)
                    // These have no CellRef values but have Unit or resolved values where
                    // template CellRefs/LinkRefs used to be.
                    || (!obj.values().any(|v| matches!(v, Value::CellRef(_)))
                        && obj.values().any(|v| matches!(v, Value::Unit) || contains_unit(v)))
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
                    // Get per-item cell values snapshot (if available)
                    let item_cell_values = per_item_snapshots.as_ref()
                        .and_then(|snapshots| snapshots.get(item_idx));
                    let Some(instantiated) = instantiate_fresh_item(
                        item,
                        list_template.element_template.as_ref(),
                        &list_template.identity.link_ref_path,
                        &initial_cell_values,
                        &mut config.initial_collections,
                        item_cell_values,
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
        // Only needed when there IS per-item remove (non-empty remove_path).
        if !remove_path.is_empty() {
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
        }

        // Toggle/set actions are encoded in evaluator config link mappings and remapped per item.

        if has_append_binding {
            let link_id = key_down_link.clone().unwrap_or_else(|| {
                panic!("[DD Interpreter] Bug: list append binding present but no List/append link");
            });
            if let Some(list_template) = list_template {
                // Template-based append (complex items with per-item reactivity)
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
                // Simple append (plain values like text, no per-item reactivity)
                if let Some(ref clear_link_id) = button_press_link {
                    config.cells.push(CellConfig {
                        id: CellId::new(&list_name),
                        initial: initial_list.clone(),
                        triggered_by: vec![LinkId::new(&link_id), LinkId::new(clear_link_id)],
                        timer_interval_ms: 0,
                        filter: EventFilter::Any,
                        transform: StateTransform::ListAppendSimpleWithClear {
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
                        transform: StateTransform::ListAppendSimple,
                        persist: true,
                    });
                }
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

        Some(config)
    } else {
        // Link-driven pattern: button |> THEN |> HOLD/LATEST
        // Task 7.1: Use evaluator-built config with dynamic trigger IDs (no hardcoded fallback)
        // The evaluator populates triggered_by from extract_link_trigger_id()
        let has_evaluator_counter_holds = evaluator_config.cells.iter()
            .any(|h| !h.triggered_by.is_empty() && h.timer_interval_ms == 0);
        let has_evaluator_link_mappings = !evaluator_config.link_mappings.is_empty();
        let has_initial_collections = !evaluator_config.initial_collections.is_empty();

        if has_evaluator_counter_holds || has_evaluator_link_mappings || has_initial_collections {
            dd_log!(
                "[DD Interpreter] Using evaluator-built config (cells: {}, link_mappings: {}, collections: {})",
                evaluator_config.cells.len(),
                evaluator_config.link_mappings.len(),
                evaluator_config.initial_collections.len()
            );
            Some(evaluator_config)
        } else {
            // Static document: no reactive behavior (no timers, no lists, no link-driven patterns).
            // Skip Worker creation — the document is fully computed from evaluation.
            dd_log!("[DD Interpreter] Static document, no Worker needed");
            None
        }
    };

    // Only create a Worker if we have reactive config
    if let Some(config) = config {
        let worker_handle = Worker::with_config(config).spawn();

        // Split returns just (event_input, task_handle) - no output channel needed
        let (event_input, task_handle) = worker_handle.split();

        // Set up global dispatcher so button clicks inject events
        let injector = EventInjector::new(event_input);
        set_global_dispatcher(injector.clone());

        // TODO(interval + interval_hold tests): Timer-driven examples fail:
        // - interval: After clear+re-run, counter="3" instead of "1". Timer ticks accumulate
        //   during page refresh cycle. The persistent DD worker and/or this timer loop may not
        //   be fully reset on clear_states + re-run.
        // - interval_hold: Shows "7" instead of "1" after 1 second. Timer fires too many rapid
        //   ticks during initialization, possibly because Timer::sleep(0) in the worker event
        //   loop processes queued timer events before the first real interval.
        // Debug: Check if set_timer_handle properly cancels the old timer on re-run.
        // Check if shutdown_persistent_worker() is called before re-initialization.
        if let Some((ref _cell_id, interval_ms)) = timer_info {
            let timer_injector = injector.clone();
            let timer_handle = Task::start_droppable(async move {
                let mut tick: u64 = 0;
                loop {
                    zoon::Timer::sleep(u32::try_from(interval_ms).expect("[DD] timer interval too large for u32")).await;
                    tick += 1;
                    timer_injector.fire_timer(super::super::core::TimerId::new(interval_ms.to_string()), tick);
                    dd_log!("[DD Timer] Tick {} for {}ms timer", tick, interval_ms);
                }
            });
            // Store timer handle separately to keep it alive
            set_timer_handle(timer_handle);
            dd_log!("[DD Interpreter] Timer started: {}ms interval", interval_ms);
        }

        // Store task handle to keep the async worker alive
        set_task_handle(task_handle);

        dd_log!("[DD Interpreter] Worker started, dispatcher configured");
    }

    // Save source code and span→ID pairs for persistence across page reloads.
    // On next load, the old AST will be re-parsed and matched against the new AST
    // so that persistence IDs remain stable (same HOLD → same localStorage key).
    if let Err(error) = local_storage().insert(DD_OLD_CODE_KEY, &source_code_for_storage) {
        zoon::eprintln!("Failed to store DD source code: {error:#?}");
    }
    if let Err(error) = local_storage().insert(
        DD_SPAN_IDS_KEY,
        &new_span_id_pairs.to_json_map().unwrap(),
    ) {
        zoon::eprintln!("Failed to store DD Span-PersistenceId pairs: {error:#}");
    }

    Some(DdResult {
        document,
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
