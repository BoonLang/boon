//! DataflowGraph → live DD collections.
//!
//! Iterates CollectionSpec entries in topological order and creates
//! real DD collections using operators from `operators.rs`.
//!
//! No Zoon, web_sys, RefCell, Mutable, or browser dependencies.

use std::collections::HashMap;
use std::sync::Arc;

use differential_dataflow::VecCollection;
use differential_dataflow::input::Input;
use differential_dataflow::input::InputSession;
use timely::dataflow::Scope;

use super::operators;
use super::types::{
    CollectionSpec, DataflowGraph, InputId, KeyedDiff, ListKey, SideEffectKind, VarId,
};
use super::value::Value;

/// Result of materializing a DataflowGraph into a live DD dataflow.
pub struct MaterializedGraph {
    /// Input sessions for event injection, keyed by InputId.
    pub input_sessions: HashMap<InputId, InputSession<u64, Value, isize>>,
    /// Internal sessions for Literal values (must be kept alive and advanced).
    pub literal_sessions: Vec<InputSession<u64, Value, isize>>,
    /// Internal sessions for LiteralList values (must be kept alive and advanced).
    pub literal_sessions_keyed: Vec<InputSession<u64, (ListKey, Value), isize>>,
    /// The VarId of the document output collection.
    pub document_var: VarId,
}

/// A DD collection that can hold either scalar Values or keyed list items.
///
/// Scalar collections contain single `Value` elements (for variables, state, etc.).
/// Keyed collections contain `(ListKey, Value)` pairs (for list operations).
/// The CollectionSpec variant determines which type is used.
enum AnyCollection<G: Scope> {
    Scalar(VecCollection<G, Value, isize>),
    Keyed(VecCollection<G, (ListKey, Value), isize>),
}

impl<G: Scope> AnyCollection<G> {
    fn as_scalar(&self) -> &VecCollection<G, Value, isize> {
        match self {
            AnyCollection::Scalar(c) => c,
            AnyCollection::Keyed(_) => panic!("Expected scalar collection, got keyed list"),
        }
    }

    fn as_keyed(&self) -> &VecCollection<G, (ListKey, Value), isize> {
        match self {
            AnyCollection::Keyed(c) => c,
            AnyCollection::Scalar(_) => panic!("Expected keyed list collection, got scalar"),
        }
    }
}

fn collection_deps_ready<G: Scope>(
    spec: &CollectionSpec,
    collections: &HashMap<VarId, AnyCollection<G>>,
) -> bool {
    let has = |var: &VarId| collections.contains_key(var);
    match spec {
        CollectionSpec::Literal(_)
        | CollectionSpec::LiteralList(_)
        | CollectionSpec::Input(_) => true,
        CollectionSpec::HoldState { initial, events, .. } => has(initial) && has(events),
        CollectionSpec::Then { source, .. }
        | CollectionSpec::Map { source, .. }
        | CollectionSpec::FlatMap { source, .. }
        | CollectionSpec::Skip { source, .. }
        | CollectionSpec::SideEffect { source, .. }
        | CollectionSpec::ListAssemble(source)
        | CollectionSpec::ListCount(source)
        | CollectionSpec::ListLatest(source)
        | CollectionSpec::ListEvery { source, .. }
        | CollectionSpec::ListAny { source, .. }
        | CollectionSpec::ListRetain { source, .. }
        | CollectionSpec::ListMap { source, .. }
        | CollectionSpec::ListMapWithKey { source, .. }
        | CollectionSpec::MapToKeyed { source, .. }
        | CollectionSpec::AppendNewKeyed { source, .. } => has(source),
        CollectionSpec::Join { left, right, .. } => has(left) && has(right),
        CollectionSpec::SampleOnEvent { event, dep, .. } => has(event) && has(dep),
        CollectionSpec::HoldLatest(sources)
        | CollectionSpec::Concat(sources)
        | CollectionSpec::CombineLatest(sources) => {
            sources.iter().all(has)
        }
        CollectionSpec::ListRetainReactive {
            list, filter_state, ..
        } => has(list) && has(filter_state),
        CollectionSpec::ListMapWithKeyReactive { source, dep, .. } => has(source) && has(dep),
        CollectionSpec::ListAppend { list, new_items } => has(list) && has(new_items),
        CollectionSpec::ListRemove { list, remove_keys } => has(list) && has(remove_keys),
        CollectionSpec::KeyedHoldState {
            initial,
            events,
            broadcasts,
            ..
        } => has(initial) && has(events) && broadcasts.as_ref().is_none_or(has),
        CollectionSpec::KeyedEventMap { items, events, .. } => has(items) && has(events),
    }
}

fn missing_collection_deps<G: Scope>(
    spec: &CollectionSpec,
    collections: &HashMap<VarId, AnyCollection<G>>,
) -> Vec<String> {
    let missing = |var: &VarId| {
        if collections.contains_key(var) {
            None
        } else {
            Some(var.as_str().to_string())
        }
    };
    match spec {
        CollectionSpec::Literal(_)
        | CollectionSpec::LiteralList(_)
        | CollectionSpec::Input(_) => Vec::new(),
        CollectionSpec::HoldState { initial, events, .. } => {
            [missing(initial), missing(events)].into_iter().flatten().collect()
        }
        CollectionSpec::Then { source, .. }
        | CollectionSpec::Map { source, .. }
        | CollectionSpec::FlatMap { source, .. }
        | CollectionSpec::Skip { source, .. }
        | CollectionSpec::SideEffect { source, .. }
        | CollectionSpec::ListAssemble(source)
        | CollectionSpec::ListCount(source)
        | CollectionSpec::ListLatest(source)
        | CollectionSpec::ListEvery { source, .. }
        | CollectionSpec::ListAny { source, .. }
        | CollectionSpec::ListRetain { source, .. }
        | CollectionSpec::ListMap { source, .. }
        | CollectionSpec::ListMapWithKey { source, .. }
        | CollectionSpec::MapToKeyed { source, .. }
        | CollectionSpec::AppendNewKeyed { source, .. } => missing(source).into_iter().collect(),
        CollectionSpec::Join { left, right, .. } => {
            [missing(left), missing(right)].into_iter().flatten().collect()
        }
        CollectionSpec::SampleOnEvent { event, dep, .. } => {
            [missing(event), missing(dep)].into_iter().flatten().collect()
        }
        CollectionSpec::HoldLatest(sources)
        | CollectionSpec::Concat(sources)
        | CollectionSpec::CombineLatest(sources) => {
            sources.iter().filter_map(missing).collect()
        }
        CollectionSpec::ListRetainReactive {
            list, filter_state, ..
        } => [missing(list), missing(filter_state)]
            .into_iter()
            .flatten()
            .collect(),
        CollectionSpec::ListMapWithKeyReactive { source, dep, .. } => {
            [missing(source), missing(dep)].into_iter().flatten().collect()
        }
        CollectionSpec::ListAppend { list, new_items } => {
            [missing(list), missing(new_items)]
                .into_iter()
                .flatten()
                .collect()
        }
        CollectionSpec::ListRemove { list, remove_keys } => {
            [missing(list), missing(remove_keys)]
                .into_iter()
                .flatten()
                .collect()
        }
        CollectionSpec::KeyedHoldState {
            initial,
            events,
            broadcasts,
            ..
        } => {
            let mut out: Vec<String> =
                [missing(initial), missing(events)].into_iter().flatten().collect();
            if let Some(broadcasts) = broadcasts {
                if let Some(dep) = missing(broadcasts) {
                    out.push(dep);
                }
            }
            out
        }
        CollectionSpec::KeyedEventMap { items, events, .. } => {
            [missing(items), missing(events)]
                .into_iter()
                .flatten()
                .collect()
        }
    }
}

/// Materialize a DataflowGraph into live DD collections within a timely scope.
///
/// Creates input sessions for external inputs, builds DD collections for each
/// CollectionSpec in topological order, and wires an inspect callback on the
/// document output collection.
///
/// Returns input sessions (for event injection) and the document VarId.
/// The `on_output` callback fires whenever the document output changes.
pub fn materialize<G>(
    graph: &DataflowGraph,
    scope: &mut G,
    on_output: impl Fn(&Value, &u64, &isize) + 'static,
    on_side_effect: Arc<dyn Fn(&SideEffectKind, &Value) + 'static>,
    on_keyed_diff: Option<Arc<dyn Fn(KeyedDiff) + 'static>>,
    on_keyed_persist: Option<Arc<dyn Fn(KeyedDiff) + 'static>>,
) -> MaterializedGraph
where
    G: Scope<Timestamp = u64>,
{
    // Create input sessions for each InputSpec
    let mut input_sessions: HashMap<InputId, InputSession<u64, Value, isize>> = HashMap::new();
    let mut input_collections: HashMap<InputId, VecCollection<G, Value, isize>> = HashMap::new();

    for input_spec in &graph.inputs {
        let (session, collection) = scope.new_collection::<Value, isize>();
        input_sessions.insert(input_spec.id, session);
        input_collections.insert(input_spec.id, collection);
    }

    // Literal sessions must be kept alive until they're advanced past the initial epoch.
    // If dropped too early, their frontier may not advance properly and data won't propagate.
    let mut literal_sessions: Vec<InputSession<u64, Value, isize>> = Vec::new();
    let mut literal_sessions_keyed: Vec<InputSession<u64, (ListKey, Value), isize>> = Vec::new();

    // Build collections once their dependencies are available.
    let mut collections: HashMap<VarId, AnyCollection<G>> = HashMap::new();
    let mut pending: Vec<(&VarId, &CollectionSpec)> = graph.collections.iter().collect();

    while !pending.is_empty() {
        let mut next_pending = Vec::new();
        let mut progressed = false;

        for (var_id, spec) in pending {
            if !collection_deps_ready(spec, &collections) {
                next_pending.push((var_id, spec));
                continue;
            }

            let any_collection: AnyCollection<G> = match spec {
            CollectionSpec::Literal(value) => {
                let (mut session, coll) = scope.new_collection::<Value, isize>();
                session.update(value.clone(), 1);
                session.flush();
                literal_sessions.push(session);
                AnyCollection::Scalar(coll)
            }

            CollectionSpec::LiteralList(items) => {
                let (mut session, coll) = scope.new_collection::<(ListKey, Value), isize>();
                for (key, value) in items {
                    session.update((key.clone(), value.clone()), 1);
                }
                session.flush();
                literal_sessions_keyed.push(session);
                AnyCollection::Keyed(coll)
            }

            CollectionSpec::Input(input_id) => AnyCollection::Scalar(
                input_collections
                    .get(input_id)
                    .expect("Input collection not found")
                    .clone(),
            ),

            CollectionSpec::HoldState {
                initial,
                events,
                initial_value,
                transform,
            } => {
                let initial_coll = collections
                    .get(initial)
                    .expect("Initial collection not found")
                    .as_scalar();
                let events_coll = collections
                    .get(events)
                    .expect("Events collection not found")
                    .as_scalar();
                let transform = Arc::clone(transform);
                AnyCollection::Scalar(operators::hold_state(
                    initial_coll,
                    events_coll,
                    initial_value.clone(),
                    move |state, event| transform(state, event),
                ))
            }

            CollectionSpec::Then { source, body } => {
                let source_coll = collections
                    .get(source)
                    .expect("Source collection not found")
                    .as_scalar();
                let body = Arc::clone(body);
                AnyCollection::Scalar(operators::then(source_coll, move |v| body(&v)))
            }

            CollectionSpec::Map { source, f } => {
                let source_coll = collections
                    .get(source)
                    .expect("Source collection not found")
                    .as_scalar();
                let f = Arc::clone(f);
                AnyCollection::Scalar(source_coll.map(move |v| f(&v)))
            }

            CollectionSpec::FlatMap { source, f } => {
                let source_coll = collections
                    .get(source)
                    .expect("Source collection not found")
                    .as_scalar();
                let f = Arc::clone(f);
                AnyCollection::Scalar(operators::when_match(source_coll, move |v| f(v)))
            }

            CollectionSpec::Join {
                left,
                right,
                combine,
            } => {
                let left_coll = collections
                    .get(left)
                    .expect("Left collection not found")
                    .as_scalar();
                let right_coll = collections
                    .get(right)
                    .expect("Right collection not found")
                    .as_scalar();
                let combine = Arc::clone(combine);
                AnyCollection::Scalar(operators::while_reactive(
                    left_coll,
                    right_coll,
                    move |a, b| combine(a, b),
                ))
            }

            CollectionSpec::SampleOnEvent { event, dep, f } => {
                let event_coll = collections
                    .get(event)
                    .expect("SampleOnEvent event not found")
                    .as_scalar();
                let dep_coll = collections
                    .get(dep)
                    .expect("SampleOnEvent dep not found")
                    .as_scalar();
                let f = Arc::clone(f);
                AnyCollection::Scalar(operators::sample_on_event(
                    event_coll,
                    dep_coll,
                    move |event, dep| f(event, dep),
                ))
            }

            CollectionSpec::HoldLatest(sources) => {
                let mut concatted = collections
                    .get(&sources[0])
                    .expect("First source not found")
                    .as_scalar()
                    .clone();
                for src in &sources[1..] {
                    let other = collections.get(src).expect("Source not found").as_scalar();
                    concatted = concatted.concat(other);
                }
                AnyCollection::Scalar(operators::hold_latest(&concatted))
            }

            CollectionSpec::Concat(sources) => {
                let mut result = collections
                    .get(&sources[0])
                    .expect("First source not found")
                    .as_scalar()
                    .clone();
                for src in &sources[1..] {
                    let other = collections.get(src).expect("Source not found").as_scalar();
                    result = result.concat(other);
                }
                AnyCollection::Scalar(result)
            }

            CollectionSpec::CombineLatest(sources) => {
                let source_colls: Vec<_> = sources
                    .iter()
                    .map(|src| {
                        collections
                            .get(src)
                            .expect("CombineLatest source not found")
                            .as_scalar()
                            .clone()
                    })
                    .collect();
                AnyCollection::Scalar(operators::combine_latest_scalars(&source_colls))
            }

            CollectionSpec::Skip { source, count } => {
                let source_coll = collections
                    .get(source)
                    .expect("Source collection not found")
                    .as_scalar();
                AnyCollection::Scalar(operators::skip(source_coll, *count))
            }

            CollectionSpec::SideEffect { source, effect } => {
                let source_coll = collections
                    .get(source)
                    .expect("SideEffect source collection not found")
                    .as_scalar();
                let effect_clone = effect.clone();
                let callback = on_side_effect.clone();
                source_coll.inspect(move |(value, _time, diff)| {
                    if *diff > 0 {
                        callback(&effect_clone, value);
                    }
                });
                // SideEffect is transparent — passes through the source collection
                AnyCollection::Scalar(source_coll.clone())
            }

            // ---------------------------------------------------------------
            // List operations — real DD operators
            // ---------------------------------------------------------------
            CollectionSpec::ListAssemble(source) => {
                let list = collections
                    .get(source)
                    .expect("ListAssemble source not found")
                    .as_keyed();
                AnyCollection::Scalar(operators::list_assemble(list))
            }

            CollectionSpec::ListCount(source) => {
                let list = collections
                    .get(source)
                    .expect("ListCount source not found")
                    .as_keyed();
                AnyCollection::Scalar(operators::list_count(list))
            }

            CollectionSpec::ListLatest(source) => {
                let list = collections
                    .get(source)
                    .expect("ListLatest source not found")
                    .as_keyed();
                AnyCollection::Scalar(operators::list_latest(list))
            }

            CollectionSpec::ListEvery { source, predicate } => {
                let list = collections
                    .get(source)
                    .expect("ListEvery source not found")
                    .as_keyed();
                let predicate = Arc::clone(predicate);
                AnyCollection::Scalar(operators::list_every(list, move |v| predicate(v)))
            }

            CollectionSpec::ListAny { source, predicate } => {
                let list = collections
                    .get(source)
                    .expect("ListAny source not found")
                    .as_keyed();
                let predicate = Arc::clone(predicate);
                AnyCollection::Scalar(operators::list_any(list, move |v| predicate(v)))
            }

            CollectionSpec::ListRetain { source, predicate } => {
                let list = collections
                    .get(source)
                    .expect("ListRetain source not found")
                    .as_keyed();
                let predicate = Arc::clone(predicate);
                AnyCollection::Keyed(operators::list_retain(list, move |v| predicate(v)))
            }

            CollectionSpec::ListRetainReactive {
                list,
                filter_state,
                predicate,
            } => {
                let list_coll = collections
                    .get(list)
                    .expect("ListRetainReactive list not found")
                    .as_keyed();
                let filter_coll = collections
                    .get(filter_state)
                    .expect("ListRetainReactive filter_state not found")
                    .as_scalar();
                let predicate = Arc::clone(predicate);
                AnyCollection::Keyed(operators::list_retain_reactive(
                    list_coll,
                    filter_coll,
                    move |v, f| predicate(v, f),
                ))
            }

            CollectionSpec::ListMap { source, f } => {
                let list = collections
                    .get(source)
                    .expect("ListMap source not found")
                    .as_keyed();
                let f = Arc::clone(f);
                AnyCollection::Keyed(operators::list_map(list, move |v| f(&v)))
            }

            CollectionSpec::ListMapWithKey { source, f } => {
                let list = collections
                    .get(source)
                    .expect("ListMapWithKey source not found")
                    .as_keyed();
                let f = Arc::clone(f);
                AnyCollection::Keyed(operators::list_map_with_key(list, move |k, v| f(k, &v)))
            }

            CollectionSpec::ListMapWithKeyReactive { source, dep, f } => {
                let list = collections
                    .get(source)
                    .expect("ListMapWithKeyReactive source not found")
                    .as_keyed();
                let dep = collections
                    .get(dep)
                    .expect("ListMapWithKeyReactive dep not found")
                    .as_scalar();
                let f = Arc::clone(f);
                AnyCollection::Keyed(operators::list_map_with_key_reactive(
                    list,
                    dep,
                    move |key, item, dep| f(key, item, dep),
                ))
            }

            CollectionSpec::ListAppend { list, new_items } => {
                let list_coll = collections
                    .get(list)
                    .expect("ListAppend list not found")
                    .as_keyed();
                let new_items_coll = collections
                    .get(new_items)
                    .expect("ListAppend new_items not found")
                    .as_keyed();
                AnyCollection::Keyed(operators::list_append(list_coll, new_items_coll))
            }

            CollectionSpec::ListRemove { list, remove_keys } => {
                let list_coll = collections
                    .get(list)
                    .expect("ListRemove list not found")
                    .as_keyed();
                let remove_coll = collections
                    .get(remove_keys)
                    .expect("ListRemove remove_keys not found")
                    .as_keyed();
                AnyCollection::Keyed(operators::list_remove(list_coll, remove_coll))
            }

            CollectionSpec::KeyedHoldState {
                initial,
                events,
                transform,
                broadcasts,
                broadcast_handler,
            } => {
                let initial_coll = collections
                    .get(initial)
                    .expect("KeyedHoldState initial not found")
                    .as_keyed();
                let events_coll = collections
                    .get(events)
                    .expect("KeyedHoldState events not found")
                    .as_keyed();
                let transform = Arc::clone(transform);
                let bcast_coll = broadcasts.as_ref().map(|var| {
                    collections
                        .get(var)
                        .expect("KeyedHoldState broadcasts not found")
                        .as_scalar()
                });
                let bcast_handler = broadcast_handler.as_ref().map(Arc::clone);
                AnyCollection::Keyed(operators::keyed_hold_state(
                    initial_coll,
                    events_coll,
                    move |s, e| transform(s, e),
                    bcast_coll,
                    bcast_handler,
                ))
            }

            CollectionSpec::MapToKeyed { source, classify } => {
                let source_coll = collections
                    .get(source)
                    .expect("MapToKeyed source not found")
                    .as_scalar();
                let classify = Arc::clone(classify);
                AnyCollection::Keyed(operators::map_to_keyed(source_coll, move |v| classify(v)))
            }

            CollectionSpec::KeyedEventMap { items, events, f } => {
                let items_coll = collections
                    .get(items)
                    .expect("KeyedEventMap items not found")
                    .as_keyed();
                let events_coll = collections
                    .get(events)
                    .expect("KeyedEventMap events not found")
                    .as_keyed();
                let f = Arc::clone(f);
                AnyCollection::Scalar(operators::keyed_event_map(
                    items_coll,
                    events_coll,
                    move |item, event| f(item, event),
                ))
            }

            CollectionSpec::AppendNewKeyed {
                source,
                f,
                initial_counter,
            } => {
                let source_coll = collections
                    .get(source)
                    .expect("AppendNewKeyed source not found")
                    .as_scalar();
                let f = Arc::clone(f);
                AnyCollection::Keyed(operators::append_new_keyed(
                    source_coll,
                    move |v| f(v),
                    *initial_counter,
                ))
            }
        };

        #[cfg(test)]
        if std::env::var_os("BOON_DD_TRACE_CRUD").is_some() {
            let name = var_id.as_str().to_string();
            let should_trace = name == "store.person_to_add"
                || name == "store.people"
                || name.contains("append_keyed")
                || name.contains("membership")
                || name.contains("keyed_hold")
                || name.contains("assemble_raw");
            if should_trace {
                match &any_collection {
                    AnyCollection::Scalar(coll) => {
                        let traced_name = name.clone();
                        coll.inspect(move |(value, time, diff)| {
                            eprintln!(
                                "[dd-trace] scalar {traced_name} @{} diff={} value={}",
                                time,
                                diff,
                                value
                            );
                        });
                    }
                    AnyCollection::Keyed(coll) => {
                        let traced_name = name.clone();
                        coll.inspect(move |((key, value), time, diff)| {
                            eprintln!(
                                "[dd-trace] keyed {traced_name} @{} diff={} key={} value={}",
                                time,
                                diff,
                                key.0,
                                value
                            );
                        });
                    }
                }
            }
        }

            collections.insert(var_id.clone(), any_collection);
            progressed = true;
        }

        if !progressed {
            let unresolved = next_pending
                .iter()
                .map(|(var_id, spec)| {
                    let missing = missing_collection_deps(spec, &collections).join(", ");
                    format!("{} <= {:?} missing [{}]", var_id.as_str(), std::mem::discriminant(*spec), missing)
                })
                .collect::<Vec<_>>()
                .join(", ");
            panic!("DD materialize stuck on unresolved collections: {unresolved}");
        }

        pending = next_pending;
    }

    // Wire inspect callback on the document output (always a scalar collection)
    let doc_collection = collections
        .get(&graph.document)
        .expect("Document collection not found")
        .as_scalar();

    doc_collection.inspect(move |(value, time, diff)| {
        on_output(value, time, diff);
    });

    // Wire keyed inspect callback on the display collection (post-retain, post-map element Values).
    // These diffs go to the bridge for O(1) per-item rendering.
    if let (Some(keyed_output), Some(on_keyed)) = (&graph.keyed_list_output, on_keyed_diff) {
        let display_coll = collections
            .get(&keyed_output.display_var)
            .expect("Keyed display collection not found")
            .as_keyed();
        let callback = on_keyed;
        display_coll.inspect(move |((key, value), _time, diff)| {
            if *diff > 0 {
                callback(KeyedDiff::Upsert {
                    key: key.clone(),
                    value: value.clone(),
                });
            } else if *diff < 0 {
                callback(KeyedDiff::Remove { key: key.clone() });
            }
        });
    }

    // Wire keyed inspect callback on the persistence collection (raw data, pre-map).
    // These diffs go to localStorage for persistence across page reloads.
    if let (Some(keyed_output), Some(on_persist)) = (&graph.keyed_list_output, on_keyed_persist) {
        let persist_coll = collections
            .get(&keyed_output.persistence_var)
            .expect("Keyed persistence collection not found")
            .as_keyed();
        let callback = on_persist;
        persist_coll.inspect(move |((key, value), _time, diff)| {
            if *diff > 0 {
                callback(KeyedDiff::Upsert {
                    key: key.clone(),
                    value: value.clone(),
                });
            } else if *diff < 0 {
                callback(KeyedDiff::Remove { key: key.clone() });
            }
        });
    }

    MaterializedGraph {
        input_sessions,
        literal_sessions,
        literal_sessions_keyed,
        document_var: graph.document.clone(),
    }
}
